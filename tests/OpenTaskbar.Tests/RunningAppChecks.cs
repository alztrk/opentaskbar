using System.Windows.Media;
using System.Windows.Media.Imaging;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class RunningAppChecks
{
    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        var resource = CreateIcon();
        var settings = CreateIcon();
        var calculator = CreateIcon();
        var generic = CreateIcon();
        const string Desktop = @"C:\Apps\Desktop.exe";
        const string Packaged = @"C:\Program Files\WindowsApps\Vendor.App_1.0.0.0_x64__abc\App.exe";
        const string FrameHost = @"C:\Windows\System32\ApplicationFrameHost.exe";
        var appIds = new Dictionary<nint, string?> { [1] = null, [2] = "Vendor.App_abc!App", [3] = "Settings!App", [4] = "Calculator!App" };
        var shellIcons = new Dictionary<string, BitmapSource> { ["Vendor.App_abc!App"] = settings, ["Settings!App"] = settings, ["Calculator!App"] = calculator };
        var resourceCalls = 0;
        var identityCalls = 0;
        var shellCalls = 0;
        var clock = new TestClock();
        var cache = new RunningAppIconCache(
            path => { resourceCalls++; return path == Desktop ? resource : null; },
            window => { identityCalls++; return appIds.GetValueOrDefault(window); },
            appId => { shellCalls++; return shellIcons.GetValueOrDefault(appId); },
            _ => generic,
            clock);

        Check(ReferenceEquals(cache.Get(Desktop, 1), resource) && identityCalls == 0,
            "Executable resources must win without querying the window identity.");
        cache.Get(Desktop, 1);
        Check(resourceCalls == 1, "Executable resources must be extracted once per executable.");

        Check(ReferenceEquals(cache.Get(Packaged, 2), settings), "A packaged executable must use its AppsFolder icon.");
        Check(!cache.HasPendingLookups, "A resolved identity icon must not schedule retries.");

        // Packaged WindowsApps executables (such as Electron Store apps) may embed a different raw .exe icon than
        // their AppxManifest/AppsFolder taskbar icon; AppsFolder must win over the raw executable resource.
        var packagedWithResource = new RunningAppIconCache(
            _ => resource,
            window => appIds.GetValueOrDefault(window),
            appId => shellIcons.GetValueOrDefault(appId),
            _ => generic,
            new TestClock());
        Check(ReferenceEquals(packagedWithResource.Get(Packaged, 2), settings),
            "A packaged WindowsApps executable with an embedded resource icon must still prefer its AppsFolder icon.");
        Check(ReferenceEquals(packagedWithResource.Get(Desktop, 1), resource),
            "A classic non-packaged executable must still prefer its embedded resource icon.");

        Check(ReferenceEquals(cache.Get(FrameHost, 3), settings) && ReferenceEquals(cache.Get(FrameHost, 4), calculator),
            "Hosted Store apps must not share an icon through their common host executable.");
        var shellCallsBefore = shellCalls;
        Check(ReferenceEquals(cache.Get(FrameHost, 3), settings) && shellCalls == shellCallsBefore,
            "A resolved identity icon must be reused without another Shell lookup.");

        appIds[5] = null;
        var identityBefore = identityCalls;
        Check(ReferenceEquals(cache.Get(FrameHost, 5), generic) && cache.HasPendingLookups,
            "A window without an identity yet must show the generic icon and wait for a retry.");
        cache.Get(FrameHost, 5);
        Check(identityCalls == identityBefore + 1, "Retries must wait for the retry delay.");
        clock.Advance(RunningAppIconCache.RetryDelay);
        appIds[5] = "Calculator!App";
        Check(ReferenceEquals(cache.Get(FrameHost, 5), calculator) && !cache.HasPendingLookups,
            "An identity published after the window appeared must replace the generic icon.");

        appIds[6] = null;
        for (var attempt = 0; attempt < RunningAppIconCache.AttemptLimit + 3; attempt++)
        {
            cache.Get(FrameHost, 6);
            clock.Advance(RunningAppIconCache.RetryDelay);
        }
        Check(identityCalls == identityBefore + 2 + RunningAppIconCache.AttemptLimit && !cache.HasPendingLookups,
            "Identity lookups must stop after the attempt limit.");

        var idleClock = new TestClock();
        var idle = new RunningAppIconCache(_ => null, _ => null, _ => null, _ => generic, idleClock);
        idle.Get(FrameHost, 7);
        idleClock.Advance(RunningAppIconCache.RetryDelay * RunningAppIconCache.AttemptLimit);
        Check(!idle.HasPendingLookups, "Retries must end for a window that is no longer asked for.");

        cache.Retain([new TrackedWindow(2, "App", Packaged)]);
        var resourceBefore = resourceCalls;
        identityBefore = identityCalls;
        Check(ReferenceEquals(cache.Get(Packaged, 2), settings) && resourceCalls == resourceBefore && identityCalls == identityBefore,
            "Tracked windows must keep their cached icons.");
        cache.Get(Desktop, 1);
        cache.Get(FrameHost, 6);
        Check(resourceCalls == resourceBefore + 2 && identityCalls == identityBefore + 1,
            "Entries of closed windows and executables must be dropped.");
        Check(cache.HasPendingLookups, "A reopened window must be looked up again.");

        // Offloaded mode: lookups run in the background and Get never blocks the dock.
        var queued = new Queue<(Func<BitmapSource?> Load, TaskCompletionSource<BitmapSource?> Done)>();
        Task<BitmapSource?> Offload(Func<BitmapSource?> load)
        {
            var done = new TaskCompletionSource<BitmapSource?>();
            queued.Enqueue((load, done));
            return done.Task;
        }
        void FinishLoads()
        {
            while (queued.TryDequeue(out var item)) item.Done.SetResult(item.Load());
        }

        var loaded = 0;
        var asyncIds = new Dictionary<nint, string?> { [2] = "Vendor.App_abc!App", [8] = "Missing!App" };
        var background = new RunningAppIconCache(
            path => path == Desktop ? resource : null,
            window => asyncIds.GetValueOrDefault(window),
            appId => shellIcons.GetValueOrDefault(appId),
            _ => generic,
            new TestClock(),
            Offload);
        background.IconLoaded += () => loaded++;

        Check(background.Get(Desktop, 1) is null && background.IsLoading,
            "A first lookup must not block the caller; there is no icon until the background load ends.");
        FinishLoads();
        Check(loaded == 1 && !background.IsLoading && ReferenceEquals(background.Get(Desktop, 1), resource),
            "A finished load must notify the caller and then serve the icon from the cache.");

        Check(background.Get(Packaged, 2) is null && background.IsLoading,
            "For a packaged executable the Start menu image is loaded first in the background.");
        Check(background.IsLoadingFor(Packaged, 2) && !background.IsLoadingFor(Desktop, 1),
            "A pending lookup must only hold back the icon of its own app.");
        // The dock refreshes (and prunes the cache with Retain) whenever a load finishes; a Store app such as
        // WhatsApp lost its freshly loaded image there and reloaded it in a loop, never showing an icon.
        background.Retain([new TrackedWindow(1, "Desktop", Desktop), new TrackedWindow(2, "App", Packaged)]);
        FinishLoads();
        background.Retain([new TrackedWindow(1, "Desktop", Desktop), new TrackedWindow(2, "App", Packaged)]);
        Check(ReferenceEquals(background.Get(Packaged, 2), settings) && !background.HasPendingLookups && !background.IsLoading,
            "A Start menu image loaded in the background must survive the refresh it triggers.");

        background.Get(FrameHost, 8);
        FinishLoads();
        background.Get(FrameHost, 8);
        FinishLoads();
        background.Get(FrameHost, 8);
        FinishLoads();
        Check(ReferenceEquals(background.Get(FrameHost, 8), generic) && background.HasPendingLookups,
            "A missing Start menu image must fall back to the file icon and keep its identity retries.");

        // Issue #83: Pinned web apps (IconLocation) and packaged Store apps (WindowsApps + AppxManifest.xml)
        {
            Check(ApplicationIconProvider.TryParseIconLocation(@"C:\Web Applications\_crx_abc\YouTube Music.ico,0", out var pwaPath, out var pwaIdx)
                  && pwaPath == @"C:\Web Applications\_crx_abc\YouTube Music.ico" && pwaIdx == 0,
                "TryParseIconLocation must parse Chrome PWA .ico paths with index 0.");
            Check(ApplicationIconProvider.TryParseIconLocation("\"%SystemRoot%\\System32\\imageres.dll\", -184", out var resPath, out var resIdx)
                  && resPath.EndsWith(@"\System32\imageres.dll", StringComparison.OrdinalIgnoreCase) && resIdx == -184,
                "TryParseIconLocation must expand environment variables, strip quotes, and parse negative resource indices.");
            Check(!ApplicationIconProvider.TryParseIconLocation("   ", out _, out _),
                "TryParseIconLocation must reject blank icon locations.");

            var fakeRoot = System.IO.Path.Combine(System.IO.Path.GetTempPath(), $"ot-winapps-{Guid.NewGuid():N}");
            try
            {
                var pkgDir = System.IO.Path.Combine(fakeRoot, "WindowsApps", "5319275A.WhatsAppDesktop_2.2500.1.0_x64__cv1g1gvanyjgm");
                System.IO.Directory.CreateDirectory(System.IO.Path.Combine(pkgDir, "Sub"));
                System.IO.File.WriteAllText(System.IO.Path.Combine(pkgDir, "AppxManifest.xml"), """
                    <?xml version="1.0" encoding="utf-8"?>
                    <Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
                      <Identity Name="5319275A.WhatsAppDesktop" Publisher="CN=Test" Version="2.2500.1.0" />
                      <Properties><DisplayName>WhatsApp</DisplayName></Properties>
                      <Applications>
                        <Application Id="App" Executable="WhatsApp.Root.exe" />
                        <Application Id="Helper" Executable="Sub\Helper.exe" />
                      </Applications>
                    </Package>
                    """);
                var mainExe = System.IO.Path.Combine(pkgDir, "WhatsApp.Root.exe");
                var helperExe = System.IO.Path.Combine(pkgDir, "Sub", "Helper.exe");
                System.IO.File.WriteAllBytes(mainExe, []);
                Check(ApplicationIconProvider.TryResolvePackagedAppUserModelId(mainExe) == "5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App",
                    "TryResolvePackagedAppUserModelId must resolve the Store app AUMID from package folder + AppxManifest.xml.");
                Check(ApplicationIconProvider.TryResolvePackagedAppUserModelId(helperExe) == "5319275A.WhatsAppDesktop_cv1g1gvanyjgm!Helper",
                    "TryResolvePackagedAppUserModelId must match nested relative executables in multi-app manifests.");
                Check(ApplicationIconProvider.TryResolvePackagedAppUserModelId(@"C:\Program Files\App\app.exe") is null,
                    "Non-WindowsApps executables must not resolve a packaged AUMID.");

                // Taskband Favorites binary blob extraction (both even and odd UTF-16 byte alignments)
                var aumidBytes = System.Text.Encoding.Unicode.GetBytes("5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App");
                var oddAligned = new byte[1 + aumidBytes.Length + 3];
                Buffer.BlockCopy(aumidBytes, 0, oddAligned, 1, aumidBytes.Length);
                var extracted = WindowsTaskbarPinImporter.ExtractAumidsFromTaskbandBytes(oddAligned);
                Check(extracted.Count == 1 && extracted[0] == "5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App",
                    "ExtractAumidsFromTaskbandBytes must find Store AUMIDs even at odd byte offsets in Taskband\\Favorites.");

                var resolvedPin = WindowsTaskbarPinImporter.TryResolvePackagedApp(
                    "5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App",
                    System.IO.Path.Combine(fakeRoot, "WindowsApps"));
                Check(resolvedPin is { DisplayName: "WhatsApp" } && string.Equals(resolvedPin.Value.ExecutablePath, mainExe, StringComparison.OrdinalIgnoreCase),
                    "TryResolvePackagedApp must resolve DisplayName and ExecutablePath from WindowsApps + AppxManifest.xml.");

                // Issue #147: Packaged/UWP app whose AppxManifest.xml has no readable .exe must resolve to shell:AppsFolder\<AUMID>.
                var uwpDir = System.IO.Path.Combine(fakeRoot, "WindowsApps", "Microsoft.OutlookForWindows_1.2025.1.0_x64__8wekyb3d8bbwe");
                System.IO.Directory.CreateDirectory(uwpDir);
                System.IO.File.WriteAllText(System.IO.Path.Combine(uwpDir, "AppxManifest.xml"), """
                    <?xml version="1.0" encoding="utf-8"?>
                    <Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
                      <Identity Name="Microsoft.OutlookForWindows" Publisher="CN=Microsoft" Version="1.2025.1.0" />
                      <Properties><DisplayName>Outlook (new)</DisplayName></Properties>
                      <Applications>
                        <Application Id="Microsoft.OutlookforWindows" Executable="$targetnametoken$.exe" />
                      </Applications>
                    </Package>
                    """);
                const string outlookAumid = "Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows";
                var resolvedUwp = WindowsTaskbarPinImporter.TryResolvePackagedApp(
                    outlookAumid,
                    System.IO.Path.Combine(fakeRoot, "WindowsApps"));
                Check(resolvedUwp is { DisplayName: "Outlook (new)" }
                      && string.Equals(resolvedUwp.Value.ExecutablePath, $"shell:AppsFolder\\{outlookAumid}", StringComparison.OrdinalIgnoreCase),
                    "TryResolvePackagedApp must emit shell:AppsFolder\\<AUMID> when AppxManifest.xml has no readable executable.");

                // Issue #147: ExtractAumidsFromTaskbandBytes captures real system/Store AUMIDs without false positives.
                const string settingsAumid = "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";
                const string telegramAumid = "TelegramMessengerLLP.TelegramDesktop_t4vj0pshhgkwm!Telegram.TelegramDesktop.Store";
                var blobText = $"\0Microsoft.Windows.Explorer\0{settingsAumid}\0{outlookAumid}\0{telegramAumid}\0FakePrefixMicrosoft.Windows.Explorer\0Microsoft.Windows.Explorer.Extra\0bad_short!App\0";
                var blobBytes = System.Text.Encoding.Unicode.GetBytes(blobText);
                var oddBlob = new byte[1 + blobBytes.Length + 2];
                Buffer.BlockCopy(blobBytes, 0, oddBlob, 1, blobBytes.Length);
                var extractedAll = WindowsTaskbarPinImporter.ExtractAumidsFromTaskbandBytes(oddBlob);
                Check(extractedAll.Count == 4
                      && extractedAll.Contains("Microsoft.Windows.Explorer", StringComparer.OrdinalIgnoreCase)
                      && extractedAll.Contains(settingsAumid, StringComparer.OrdinalIgnoreCase)
                      && extractedAll.Contains(outlookAumid, StringComparer.OrdinalIgnoreCase)
                      && extractedAll.Contains(telegramAumid, StringComparer.OrdinalIgnoreCase),
                    "ExtractAumidsFromTaskbandBytes must capture File Explorer and Store/System AUMIDs without false positives.");

                // Issue #147: Non-!App AUMIDs in AppShortcutResolver must route to shell:AppsFolder\<AUMID>.
                var outlookSpec = AppShortcutResolver.Resolve(null, outlookAumid, "Outlook");
                Check(string.Equals(outlookSpec.Target, $"shell:AppsFolder\\{outlookAumid}", StringComparison.OrdinalIgnoreCase),
                    "AppShortcutResolver must resolve non-!App packaged AUMIDs (Outlook) to shell:AppsFolder\\<AUMID>.");
                var telegramSpec = AppShortcutResolver.Resolve(null, telegramAumid, "Telegram");
                Check(string.Equals(telegramSpec.Target, $"shell:AppsFolder\\{telegramAumid}", StringComparison.OrdinalIgnoreCase),
                    "AppShortcutResolver must resolve non-!App packaged AUMIDs (Telegram Store) to shell:AppsFolder\\<AUMID>.");
                var settingsSpec = AppShortcutResolver.Resolve($"shell:AppsFolder\\{settingsAumid}", null, "Settings");
                Check(string.Equals(settingsSpec.Target, $"shell:AppsFolder\\{settingsAumid}", StringComparison.OrdinalIgnoreCase),
                    "AppShortcutResolver must preserve pre-resolved shell:AppsFolder\\<AUMID> targets.");

                // Issue #147: PWA shortcuts must preserve their .lnk / arguments rather than resolving to plain Edge/Chrome.
                var fakeProxy = System.IO.Path.Combine(fakeRoot, "msedge_proxy.exe");
                var pwaLinkA = System.IO.Path.Combine(fakeRoot, "YouTube.lnk");
                var pwaLinkB = System.IO.Path.Combine(fakeRoot, "Spotify.lnk");
                System.IO.File.WriteAllBytes(fakeProxy, []);
                System.IO.File.WriteAllBytes(pwaLinkA, []);
                System.IO.File.WriteAllBytes(pwaLinkB, []);
                const string pwaArgsA = "--profile-directory=Default --app-id=aaaaaaaa";
                const string pwaArgsB = "--profile-directory=Default --app-id=bbbbbbbb";
                var pwaSpecWithLink = AppShortcutResolver.Resolve(fakeProxy, "MSEdge._crx_aaaaaaaa", "YouTube", pwaArgsA, pwaLinkA);
                Check(string.Equals(pwaSpecWithLink.Target, pwaLinkA, StringComparison.OrdinalIgnoreCase),
                    "AppShortcutResolver must preserve the PWA .lnk shortcut instead of redirecting to plain Edge/Chrome.");
                var pwaSpecNoLink = AppShortcutResolver.Resolve(fakeProxy, "MSEdge._crx_aaaaaaaa", "YouTube", pwaArgsA, null);
                Check(string.Equals(pwaSpecNoLink.Target, fakeProxy, StringComparison.OrdinalIgnoreCase)
                      && pwaSpecNoLink.Arguments == pwaArgsA,
                    "AppShortcutResolver must preserve executable and launch arguments when no .lnk file is present.");

                // Issue #147: Multi-arg PWA deduplication and distinct dock keys.
                var dedup = new WindowsTaskbarPinImporter.PinDeduplicator();
                var pwaPinA = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "YouTube", pwaLinkA, fakeProxy, pwaArgsA, null,
                    new WindowsTaskbarPinImporter.ShellLinkMetadata(fakeProxy, pwaArgsA, null, "MSEdge._crx_aaaaaaaa", null));
                var pwaPinB = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "Spotify", pwaLinkB, fakeProxy, pwaArgsB, null,
                    new WindowsTaskbarPinImporter.ShellLinkMetadata(fakeProxy, pwaArgsB, null, "MSEdge._crx_bbbbbbbb", null));
                var pwaPinADup = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "YouTube Copy", pwaLinkA, fakeProxy, "  --profile-directory=Default   --app-id=aaaaaaaa ", null,
                    new WindowsTaskbarPinImporter.ShellLinkMetadata(fakeProxy, pwaArgsA, null, "MSEdge._crx_aaaaaaaa", null));
                Check(pwaPinA is not null && pwaPinB is not null && pwaPinADup is not null,
                    "ResolveShortcutEntry must resolve distinct argument-bearing PWA shortcuts.");
                Check(dedup.TryAddShortcut(pwaPinA!) && dedup.TryAddShortcut(pwaPinB!) && !dedup.TryAddShortcut(pwaPinADup!),
                    "PinDeduplicator must keep distinct PWAs sharing msedge_proxy.exe while deduplicating identical normalized args/AUMID.");

                var dockPwaA = new DockApp("YouTube", fakeProxy) { LaunchArguments = pwaArgsA, AppUserModelId = "MSEdge._crx_aaaaaaaa" };
                var dockPwaB = new DockApp("Spotify", fakeProxy) { LaunchArguments = pwaArgsB, AppUserModelId = "MSEdge._crx_bbbbbbbb" };
                var pwaSettings = new DockSettings(PinOrder: [dockPwaB.DockKey, dockPwaA.DockKey]);
                var visiblePwaKeys = OpenTaskbar.MainWindow.GetVisiblePinnedPaths([dockPwaA, dockPwaB], pwaSettings);
                Check(visiblePwaKeys.Count == 2
                      && string.Equals(visiblePwaKeys[0], dockPwaB.DockKey, StringComparison.OrdinalIgnoreCase)
                      && string.Equals(visiblePwaKeys[1], dockPwaA.DockKey, StringComparison.OrdinalIgnoreCase),
                    "Distinct PWAs sharing msedge_proxy.exe must have distinct dock keys and preserve PinOrder.");
                Check(ReferenceEquals(OpenTaskbar.MainWindow.FindVisibleDockApp([dockPwaA, dockPwaB], [], pwaSettings, dockPwaA.DockKey), dockPwaA)
                      && ReferenceEquals(OpenTaskbar.MainWindow.FindVisibleDockApp([dockPwaA, dockPwaB], [], pwaSettings, dockPwaB.DockKey), dockPwaB),
                    "FindVisibleDockApp must resolve each PWA independently without colliding on ExecutablePath.");

                // Issue #147: File Explorer empty-TargetPath .lnk resolution via CLSID / AUMID, and packaged .lnk via IPropertyStore.
                var explorerViaClsid = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "File Explorer", System.IO.Path.Combine(fakeRoot, "File Explorer.lnk"),
                    string.Empty, string.Empty, string.Empty,
                    new WindowsTaskbarPinImporter.ShellLinkMetadata(null, null, null, null, "::{52205fd8-5dfb-447d-801a-d0b52f2e83e1}"));
                Check(explorerViaClsid is not null
                      && explorerViaClsid.ExecutablePath.EndsWith(@"\explorer.exe", StringComparison.OrdinalIgnoreCase)
                      && explorerViaClsid.AppUserModelId is null,
                    "File Explorer shortcuts with empty TargetPath and Explorer CLSID must resolve to explorer.exe.");
                var explorerViaAumid = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "Dosya Gezgini", System.IO.Path.Combine(fakeRoot, "Dosya Gezgini.lnk"),
                    string.Empty, string.Empty, string.Empty,
                    new WindowsTaskbarPinImporter.ShellLinkMetadata(null, null, null, "Microsoft.Windows.Explorer", null));
                Check(explorerViaAumid is not null
                      && explorerViaAumid.ExecutablePath.EndsWith(@"\explorer.exe", StringComparison.OrdinalIgnoreCase),
                    "File Explorer shortcuts identified by Microsoft.Windows.Explorer AUMID must resolve to explorer.exe.");
                var packagedViaLnk = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "Outlook", System.IO.Path.Combine(fakeRoot, "Outlook.lnk"),
                    string.Empty, string.Empty, string.Empty,
                    new WindowsTaskbarPinImporter.ShellLinkMetadata(null, null, null, outlookAumid, null),
                    System.IO.Path.Combine(fakeRoot, "WindowsApps"));
                Check(packagedViaLnk is not null
                      && string.Equals(packagedViaLnk.ExecutablePath, $"shell:AppsFolder\\{outlookAumid}", StringComparison.OrdinalIgnoreCase)
                      && string.Equals(packagedViaLnk.AppUserModelId, outlookAumid, StringComparison.OrdinalIgnoreCase),
                    "Packaged Store .lnk shortcuts with empty TargetPath must resolve via PKEY_AppUserModel_ID.");

                // Issue #147: .url shortcut parsing and AUMID-aware running window matching/exclusion.
                var urlFile = System.IO.Path.Combine(fakeRoot, "Game.url");
                System.IO.File.WriteAllText(urlFile, "[InternetShortcut]\r\nURL=steam://rungameid/730\r\nIconFile=C:\\Icons\\cs.ico\r\nIconIndex=0\r\n");
                var urlPin = WindowsTaskbarPinImporter.TryResolveUrlShortcut(urlFile);
                Check(urlPin is { DisplayName: "Game", IconLocation: @"C:\Icons\cs.ico,0" }
                      && string.Equals(urlPin.ShortcutPath, urlFile, StringComparison.OrdinalIgnoreCase),
                    "TryResolveUrlShortcut must parse URL and IconLocation from .url shortcuts.");

                var pinnedOutlook = new DockApp("Outlook", $"shell:AppsFolder\\{outlookAumid}") { AppUserModelId = outlookAumid };
                var outlookWindow = new TrackedWindow(42, "Inbox - Outlook", FrameHost, AppUserModelId: outlookAumid);
                var otherUwpWindow = new TrackedWindow(43, "Settings", FrameHost, AppUserModelId: settingsAumid);
                Check(OpenTaskbar.MainWindow.MatchesWindow(pinnedOutlook, outlookWindow)
                      && !OpenTaskbar.MainWindow.MatchesWindow(pinnedOutlook, otherUwpWindow),
                    "MatchesWindow must match shell:AppsFolder pins to hosted windows by AUMID without colliding on ApplicationFrameHost.exe.");
                var projectedWithAumidExclude = RunningAppProjection.Project(
                    [outlookWindow, otherUwpWindow],
                    new HashSet<string>(StringComparer.OrdinalIgnoreCase) { pinnedOutlook.ExecutablePath },
                    foreground: 42,
                    notificationBadgesEnabled: false,
                    static _ => true,
                    new HashSet<string>(StringComparer.OrdinalIgnoreCase) { outlookAumid });
                Check(projectedWithAumidExclude.Count == 1
                      && string.Equals(projectedWithAumidExclude[0].AppUserModelId, settingsAumid, StringComparison.OrdinalIgnoreCase),
                    "RunningAppProjection must exclude already-pinned packaged apps by AUMID while keeping other hosted UWP apps.");

                // #173: a stale/unactivatable window must launch instead of silently doing nothing on a dock click.
                var launches = 0;
                var warnings = 0;
                Check(OpenTaskbar.MainWindow.HandleActivationResult(true, () => launches++, _ => warnings++)
                      && launches == 0 && warnings == 0,
                    "A successful activation must keep the current window and launch nothing.");
                Check(!OpenTaskbar.MainWindow.HandleActivationResult(false, () => launches++, _ => warnings++)
                      && launches == 1 && warnings == 1,
                    "A failed activation must launch exactly one new instance and log one warning.");

                // Issue #167: Inbox system UWP packages (C:\Windows\ImmersiveControlPanel\SystemSettings.exe and
                // C:\Windows\SystemApps\*) must resolve their packaged AUMID and route through shell:AppsFolder\<AUMID>
                // instead of falling back to the generic .exe icon.
                Check(ApplicationIconProvider.ComputePublisherId("CN=Microsoft Windows, O=Microsoft Corporation, L=Redmond, S=Washington, C=US") == "cw5n1h2txyewy",
                    "ComputePublisherId must match Windows' AppX PublisherId for Microsoft Windows.");
                Check(ApplicationIconProvider.ComputePublisherId("CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US") == "8wekyb3d8bbwe",
                    "ComputePublisherId must match Windows' AppX PublisherId for Microsoft Corporation.");

                var immersiveDir = System.IO.Path.Combine(fakeRoot, "Windows", "ImmersiveControlPanel");
                System.IO.Directory.CreateDirectory(immersiveDir);
                System.IO.File.WriteAllText(System.IO.Path.Combine(immersiveDir, "AppxManifest.xml"), """
                    <?xml version="1.0" encoding="utf-8"?>
                    <Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
                      <Identity Name="windows.immersivecontrolpanel" Publisher="CN=Microsoft Windows, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" Version="10.0.8.1000" />
                      <Properties><DisplayName>Settings</DisplayName></Properties>
                      <Applications>
                        <Application Id="microsoft.windows.immersivecontrolpanel" Executable="SystemSettings.exe" EntryPoint="AppObject.EntryPoint" />
                      </Applications>
                    </Package>
                    """);
                var fakeSettingsExe = System.IO.Path.Combine(immersiveDir, "SystemSettings.exe");
                System.IO.File.WriteAllBytes(fakeSettingsExe, []);
                Check(string.Equals(ApplicationIconProvider.TryResolvePackagedAppUserModelId(fakeSettingsExe), settingsAumid, StringComparison.OrdinalIgnoreCase),
                    "TryResolvePackagedAppUserModelId must resolve Windows Settings AUMID from ImmersiveControlPanel\\AppxManifest.xml.");

                var systemAppDir = System.IO.Path.Combine(fakeRoot, "Windows", "SystemApps", "Microsoft.Windows.ShellExperienceHost_cw5n1h2txyewy");
                System.IO.Directory.CreateDirectory(systemAppDir);
                System.IO.File.WriteAllText(System.IO.Path.Combine(systemAppDir, "AppxManifest.xml"), """
                    <?xml version="1.0" encoding="utf-8"?>
                    <Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
                      <Identity Name="Microsoft.Windows.ShellExperienceHost" Publisher="CN=Microsoft Windows, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" Version="10.0.0.0" />
                      <Properties><DisplayName>Shell Experience</DisplayName></Properties>
                      <Applications>
                        <Application Id="App" Executable="ShellExperienceHost.exe" EntryPoint="ShellExperienceHost.App" />
                      </Applications>
                    </Package>
                    """);
                var fakeShellHostExe = System.IO.Path.Combine(systemAppDir, "ShellExperienceHost.exe");
                System.IO.File.WriteAllBytes(fakeShellHostExe, []);
                Check(string.Equals(ApplicationIconProvider.TryResolvePackagedAppUserModelId(fakeShellHostExe), "Microsoft.Windows.ShellExperienceHost_cw5n1h2txyewy!App", StringComparison.OrdinalIgnoreCase),
                    "TryResolvePackagedAppUserModelId must resolve SystemApps 2-part package folder AUMIDs.");

                // Pure UWP/AppContainer EntryPoint with a readable .exe must still resolve to shell:AppsFolder\<AUMID>,
                // whereas Windows.FullTrustApplication keeps its readable desktop bridge executable.
                var uwpAppContainerDir = System.IO.Path.Combine(fakeRoot, "WindowsApps", "windows.immersivecontrolpanel_10.0.8.1000_neutral_neutral_cw5n1h2txyewy");
                System.IO.Directory.CreateDirectory(uwpAppContainerDir);
                System.IO.File.WriteAllText(System.IO.Path.Combine(uwpAppContainerDir, "AppxManifest.xml"), """
                    <?xml version="1.0" encoding="utf-8"?>
                    <Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
                      <Identity Name="windows.immersivecontrolpanel" Publisher="CN=Microsoft Windows, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" Version="10.0.8.1000" />
                      <Properties><DisplayName>Settings</DisplayName></Properties>
                      <Applications>
                        <Application Id="microsoft.windows.immersivecontrolpanel" Executable="SystemSettings.exe" EntryPoint="AppObject.EntryPoint" />
                      </Applications>
                    </Package>
                    """);
                System.IO.File.WriteAllBytes(System.IO.Path.Combine(uwpAppContainerDir, "SystemSettings.exe"), []);
                var resolvedSettingsPkg = WindowsTaskbarPinImporter.TryResolvePackagedApp(
                    settingsAumid,
                    System.IO.Path.Combine(fakeRoot, "WindowsApps"));
                Check(resolvedSettingsPkg is { DisplayName: "Settings" }
                      && string.Equals(resolvedSettingsPkg.Value.ExecutablePath, $"shell:AppsFolder\\{settingsAumid}", StringComparison.OrdinalIgnoreCase),
                    "TryResolvePackagedApp must emit shell:AppsFolder\\<AUMID> for AppContainer UWP EntryPoints even when the raw .exe is readable.");

                var fullTrustDir = System.IO.Path.Combine(fakeRoot, "WindowsApps", "Vendor.FullTrust_1.0.0.0_x64__8wekyb3d8bbwe");
                System.IO.Directory.CreateDirectory(fullTrustDir);
                System.IO.File.WriteAllText(System.IO.Path.Combine(fullTrustDir, "AppxManifest.xml"), """
                    <?xml version="1.0" encoding="utf-8"?>
                    <Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
                      <Identity Name="Vendor.FullTrust" Publisher="CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US" Version="1.0.0.0" />
                      <Properties><DisplayName>FullTrust App</DisplayName></Properties>
                      <Applications>
                        <Application Id="App" Executable="FullTrust.exe" EntryPoint="Windows.FullTrustApplication" />
                      </Applications>
                    </Package>
                    """);
                var fullTrustExe = System.IO.Path.Combine(fullTrustDir, "FullTrust.exe");
                System.IO.File.WriteAllBytes(fullTrustExe, []);
                var resolvedFullTrust = WindowsTaskbarPinImporter.TryResolvePackagedApp(
                    "Vendor.FullTrust_8wekyb3d8bbwe!App",
                    System.IO.Path.Combine(fakeRoot, "WindowsApps"));
                Check(resolvedFullTrust is { DisplayName: "FullTrust App" }
                      && string.Equals(resolvedFullTrust.Value.ExecutablePath, fullTrustExe, StringComparison.OrdinalIgnoreCase),
                    "TryResolvePackagedApp must keep the readable executable for Windows.FullTrustApplication desktop bridges.");

                // Legacy .lnk pointing directly to ImmersiveControlPanel\SystemSettings.exe without PKEY_AppUserModel_ID
                // must upgrade to shell:AppsFolder\<settingsAumid> and preserve the Settings AUMID.
                var settingsPinViaRawExe = WindowsTaskbarPinImporter.ResolveShortcutEntry(
                    "Ayarlar",
                    System.IO.Path.Combine(fakeRoot, "Ayarlar.lnk"),
                    fakeSettingsExe,
                    string.Empty,
                    string.Empty,
                    metadata: null,
                    windowsAppsRoot: System.IO.Path.Combine(fakeRoot, "WindowsApps"));
                Check(settingsPinViaRawExe is not null
                      && string.Equals(settingsPinViaRawExe.ExecutablePath, $"shell:AppsFolder\\{settingsAumid}", StringComparison.OrdinalIgnoreCase)
                      && string.Equals(settingsPinViaRawExe.AppUserModelId, settingsAumid, StringComparison.OrdinalIgnoreCase),
                    "ResolveShortcutEntry must upgrade ImmersiveControlPanel\\SystemSettings.exe pins to shell:AppsFolder\\<AUMID>.");

                Check(DockApp.IsPackagedInstallPath(fakeSettingsExe)
                      && DockApp.IsSystemPackagedInstallPath(fakeSettingsExe)
                      && DockApp.IsPackagedInstallPath(fakeShellHostExe)
                      && DockApp.IsPackagedInstallPath(mainExe)
                      && !DockApp.IsSystemPackagedInstallPath(mainExe)
                      && !DockApp.IsPackagedInstallPath(Desktop),
                    "DockApp packaged install path helpers must classify WindowsApps, SystemApps, ImmersiveControlPanel, and classic desktop paths.");
                Check(DockApp.IsSelfExecutable(@"C:\Program Files\OpenTaskbar\OpenTaskbar.exe", @"C:\Program Files\OpenTaskbar\OpenTaskbar.exe")
                      && DockApp.IsSelfExecutable(@"D:\Other\OpenTaskbar.exe", @"C:\Program Files\OpenTaskbar\OpenTaskbar.exe")
                      && !DockApp.IsSelfExecutable(Desktop, @"C:\Program Files\OpenTaskbar\OpenTaskbar.exe"),
                    "DockApp.IsSelfExecutable must identify OpenTaskbar's own executable without matching unrelated apps.");
            }
            finally
            {
                if (System.IO.Directory.Exists(fakeRoot)) System.IO.Directory.Delete(fakeRoot, recursive: true);
            }
        }

        // Two AUMID groups can share one executable (e.g. LibreWolf). Indicator aggregation must not throw
        // a duplicate-key exception during startup and must combine counts and foreground state.
        {
            const string shared = @"C:\Program Files\LibreWolf\librewolf.exe";
            var candidates = new[]
            {
                new RunningAppCandidate(shared, [new TrackedWindow(10, "A", shared)], false, false, "LibreWolf.A!App"),
                new RunningAppCandidate(shared, [new TrackedWindow(11, "B", shared), new TrackedWindow(12, "C", shared)], false, true, "LibreWolf.B!App")
            };
            var states = RunningAppProjection.AggregateByExecutablePath(candidates);
            Check(states.Count == 1 && states[shared].WindowCount == 3 && states[shared].IsForeground,
                "Duplicate executable paths from distinct AUMID groups must aggregate instead of throwing.");
            Check(RunningAppProjection.AggregateByExecutablePath([]).Count == 0,
                "An empty window snapshot must produce no indicator states.");
        }

        return checks;
    }

    private static BitmapSource CreateIcon()
    {
        var icon = BitmapSource.Create(1, 1, 96, 96, PixelFormats.Bgra32, null, new byte[4], 4);
        icon.Freeze();
        return icon;
    }
}
