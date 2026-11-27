using System.IO;
using System.Text;
using System.Text.RegularExpressions;
using System.Windows;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class ShellFlyoutPositionChecks
{
    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        var native = new ShellShortcuts.SearchWindow(101, 42, "TestShell", "TestWindow");
        var legacyLookups = 0;
        List<nint> FindLegacy()
        {
            legacyLookups++;
            return [202, 303];
        }

        var selected = ShellShortcuts.SelectSearchWindows(native, FindLegacy);
        Check(selected.Native == native && selected.Legacy.Count == 0 && legacyLookups == 0,
            "A UIA-confirmed Search surface must take precedence over the legacy lookup.");
        selected = ShellShortcuts.SelectSearchWindows(null, FindLegacy);
        Check(selected.Native is null && selected.Legacy.SequenceEqual([(nint)202, (nint)303]) && legacyLookups == 1,
            "The legacy lookup must preserve auxiliary Search windows when no native surface is confirmed.");
        selected = ShellShortcuts.SelectSearchWindows(null, () => []);
        Check(selected.Native is null && selected.Legacy.Count == 0,
            "Unknown Search surfaces must leave both targeting paths empty.");
        Check(ShellShortcuts.HasSearchIdentity("SearchTextBox", "Suchfeld")
            && ShellShortcuts.HasSearchIdentity("", "Windows Search")
            && !ShellShortcuts.HasSearchIdentity("MainWindow", "File Explorer"),
            "Search identity must accept semantic automation IDs without a localized title.");

        for (var successfulStep = 1; successfulStep <= 4; successfulStep++)
        {
            var calls = new List<int>();
            bool Apply(int step)
            {
                calls.Add(step);
                return step == successfulStep;
            }

            var applied = ShellShortcuts.TryApplySearchQuery(() => Apply(1), () => Apply(2), () => Apply(3));
            Check(applied == (successfulStep < 4) && calls.SequenceEqual(Enumerable.Range(1, Math.Min(3, successfulStep))),
                "Query synchronization must try ValuePattern, the identified editor, and keyboard input in order.");
        }

        var host = new Rect(-1600, 200, 900, 720);
        var panel = new Rect(-1570, 240, 600, 460);
        var work = new Rect(-1920, 0, 1920, 1080);
        var anchor = new Rect(-1300, 1000, 50, 50);
        var bottom = ShellShortcuts.ComputeMeasuredSearchPosition(host, panel, work, anchor, DockEdge.Bottom, 1.5);
        Check(bottom == (-1330, 482),
            "Measured Search must align its visible panel above the button with a DPI-scaled gap.");
        var resized = ShellShortcuts.ComputeMeasuredSearchPosition(host, new Rect(-1570, 240, 600, 560),
            work, anchor, DockEdge.Bottom, 1.5);
        Check(resized.X == bottom.X && resized.Y == bottom.Y - 100,
            "Growing Search results must keep the visible bottom edge anchored.");
        var left = ShellShortcuts.ComputeMeasuredSearchPosition(host, panel, work,
            new Rect(-1920, 400, 50, 50), DockEdge.Left, 1);
        var right = ShellShortcuts.ComputeMeasuredSearchPosition(host, panel, work,
            new Rect(-50, 400, 50, 50), DockEdge.Right, 1);
        Check(left.X + 30 == -1858 && right.X + 30 + 600 == -62 && left.Y == right.Y,
            "Measured side-dock Search must apply the gap to the visible panel on either side.");
        var clamped = ShellShortcuts.ComputeMeasuredSearchPosition(host, panel, work,
            new Rect(-50, 1000, 50, 50), DockEdge.Bottom, 1);
        Check(clamped.X + 30 + 600 == -8,
            "Measured Search must clamp its visible panel even when the host extends past the work area.");

        var processIds = new Dictionary<nint, uint> { [11] = 100, [22] = 200, [33] = 300 };
        var moved = new List<(nint Handle, int X, int Y)>();
        var persisted = 0;
        var cleared = 0;
        var registry = new ShellFlyoutPositionRegistry(
            handle => processIds.GetValueOrDefault(handle),
            handle => processIds.ContainsKey(handle),
            (handle, x, y) => moved.Add((handle, x, y)),
            _ => persisted++,
            () => cleared++);

        var generation = registry.Generation;
        registry.Remember(11, 40, 900);
        // The same window is anchored again on a later open; the original position must be kept.
        registry.Remember(11, 10, 20);
        registry.Remember(22, 70, 800);
        registry.Remember(33, 90, 700);
        Check(registry.Generation == generation, "Remembering positions must not invalidate in-flight reposition loops.");

        registry.Remember(44, 120, 600);
        Check(persisted == 4, "Every newly remembered flyout is persisted for crash recovery.");
        // Simulate Windows closing Search and reusing its HWND for a different process after the last sighting.
        processIds.Remove(22);
        processIds[22] = 999;
        registry.Restore();

        Check(cleared == 0, "A journal with a recycled handle is kept for a later recovery pass.");
        Check(moved.Count == 2, "Only still-valid shell windows are restored; recycled and closed handles are skipped.");
        Check(moved[0] == (11, 40, 900), "The first recorded position is restored, not the most recent anchored one.");
        Check(moved[1] == (33, 90, 700), "Every surviving flyout is restored, including Control Center.");
        Check(registry.Generation == generation + 1, "Restore invalidates any reposition loop still running.");

        registry.Restore();
        Check(moved.Count == 2, "Restoring twice must not move the windows again.");

        // The journal survives a hard kill, when none of the in-process state above is available.
        var journal = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName() + ".txt");
        try
        {
            File.WriteAllText(journal, "12345 100 40 900\nbroken line\n67890 200 70 800\n");
            var recovered = ShellFlyoutPositionJournal.Read(journal);
            Check(recovered.Count == 2, "Malformed journal lines are skipped instead of moving windows blindly.");
            Check(recovered[0] == new FlyoutPosition(12345, 100, 40, 900)
                  && recovered[1] == new FlyoutPosition(67890, 200, 70, 800),
                "The journal round-trips handle, process id and the original position.");

            ShellFlyoutPositionJournal.Write([new FlyoutPosition(111, 7, 5, 6)], journal);
            Check(ShellFlyoutPositionJournal.Read(journal).SequenceEqual([new FlyoutPosition(111, 7, 5, 6)]),
                "Journal writes are atomic and readable by the watchdog.");
            ShellFlyoutPositionJournal.Delete(journal);
            Check(!File.Exists(journal), "A successful restore removes the recovery journal.");
        }
        finally
        {
            File.Delete(journal);
            File.Delete(journal + ".tmp");
        }

        // Faz 3: the exact wire format shared with Rust `flyout_writer`/`flyout_journal` and the C watchdog.
        var wire = Path.Combine(Path.GetTempPath(), "ot-flyout-wire-" + Guid.NewGuid().ToString("N") + ".txt");
        try
        {
            var positions = Enumerable.Range(1, 17)
                .Select(i => new FlyoutPosition(1000 + i, (uint)(7 + i), -320 - i, 17 + i))
                .ToArray();
            ShellFlyoutPositionJournal.Write(positions, wire);
            var raw = File.ReadAllBytes(wire);
            Check(!(raw.Length >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF),
                "The flyout journal must be UTF-8 without a BOM.");
            var text = Encoding.UTF8.GetString(raw);
            Check(!text.Contains('\r'), "The flyout journal must use Rust's LF line endings.");
            var lines = text.Split('\n', StringSplitOptions.RemoveEmptyEntries);
            Check(lines.Length == ShellFlyoutPositionJournal.MaxEntries,
                "The flyout journal must keep at most 16 entries even when given 17.");
            Check(lines[0] == "1001 8 -321 18",
                "Fields must be invariant <hwnd> <pid> <left> <top>, including negative coordinates.");
            Check(lines.All(line => Regex.IsMatch(line, @"^\d+ \d+ -?\d+ -?\d+$")),
                "Every journal line must be parseable by the watchdog's %llu %u %d %d scan.");
            Check(ShellFlyoutPositionJournal.TemporaryFilePath(wire) == wire + ".tmp"
                  && !File.Exists(wire + ".tmp"),
                "Both writers must use flyout-positions.txt.tmp and leave no stale temp file.");
            Check(ShellFlyoutPositionJournal.Read(wire).Count == 16, "The capped journal must round-trip.");

            // Rust may append generation/seq metadata; the watchdog still only reads the first four fields.
            File.WriteAllText(wire, "1234 56 -320 17 generation=11 seq=29\n1234 56 -320 17 generation=11\n0 56 1 2\n");
            var extended = ShellFlyoutPositionJournal.Read(wire);
            Check(extended.Count == 1 && extended[0] == new FlyoutPosition(1234, 56, -320, 17),
                "A complete Rust metadata suffix must read; half-written metadata and HWND 0 must be skipped.");

            // HWNDs are recycled: PID mismatches must never move an unrelated window.
            Check(ShellFlyoutPositionJournal.IsCurrent(extended[0], _ => 56, _ => true)
                  && !ShellFlyoutPositionJournal.IsCurrent(extended[0], _ => 99, _ => true)
                  && !ShellFlyoutPositionJournal.IsCurrent(extended[0], _ => 56, _ => false)
                  && !ShellFlyoutPositionJournal.IsCurrent(new FlyoutPosition(1, 0, 0, 0), _ => 0, _ => true),
                "Recovery must require a live handle still owned by the recorded PID.");
            ShellFlyoutPositionJournal.Delete(wire);
            Check(!File.Exists(wire) && !File.Exists(wire + ".tmp"),
                "A verified restore must remove both the journal and its temporary file.");
        }
        finally
        {
            File.Delete(wire);
            File.Delete(wire + ".tmp");
        }

        // Side-dock Start and Search flyout clamping: top, middle, and bottom anchors on tall side docks
        // must never place the flyout outside the work area.
        const int workL = 0, workT = 0, workR = 2560, workB = 1440;
        const int searchW = 780, searchH = 740;
        foreach (var edge in new[] { OpenTaskbar.Models.DockEdge.Left, OpenTaskbar.Models.DockEdge.Right })
        {
            var anchorL = edge == OpenTaskbar.Models.DockEdge.Left ? 0.0 : 2504.0;
            var anchorR = edge == OpenTaskbar.Models.DockEdge.Left ? 56.0 : 2560.0;
            foreach (var anchorY in new[] { 12.0, 700.0, 1380.0 })
            {
                var (sx, sy) = ShellShortcuts.ComputeSearchPosition(
                    searchW, searchH, workL, workT, workR, workB,
                    anchorL, anchorY, anchorR, anchorY + 44, edge);
                Check(sx >= workL + 8 && sx + searchW <= workR - 8,
                    $"Search X must stay inside work area for {edge} at anchorY={anchorY}.");
                Check(sy >= workT + 8 && sy + searchH <= workB - 8,
                    $"Search Y must stay inside work area for {edge} at anchorY={anchorY}.");

                var (stx, sty) = ShellShortcuts.ComputeStartMenuPosition(
                    642, 724, workL, workT, workR, workB,
                    anchorL, anchorY, anchorR, anchorY + 44, edge, dpiScale: 1.0);
                Check(stx >= workL + 8 && stx + 642 <= workR - 8,
                    $"Discrete Start X must stay inside work area for {edge} at anchorY={anchorY}.");
                Check(sty >= workT + 8 && sty + 724 <= workB - 8,
                    $"Discrete Start Y must stay inside work area for {edge} at anchorY={anchorY}.");

                // Full-screen canvas Start menu: verify resolved flyout rect stays strictly inside work area.
                var (cx, cy) = ShellShortcuts.ComputeStartMenuPosition(
                    2560, 1440, workL, workT, workR, workB,
                    anchorL, anchorY, anchorR, anchorY + 44, edge, dpiScale: 1.25);
                var flyW = (int)Math.Round(642 * 1.25);
                var flyH = (int)Math.Round(724 * 1.25);
                var localX = (2560 - flyW) / 2;
                var localY = 1440 - flyH - (int)Math.Round(12 * 1.25);
                var resolvedX = cx + localX;
                var resolvedY = cy + localY;
                Check(resolvedX >= workL + 8 && resolvedX + flyW <= workR - 8,
                    $"Canvas Start flyout X must stay inside work area for {edge} at anchorY={anchorY}.");
                Check(resolvedY >= workT + 8 && resolvedY + flyH <= workB - 8,
                    $"Canvas Start flyout Y must stay inside work area for {edge} at anchorY={anchorY}.");
            }
        }

        // Bottom-edge measured Start menu: varying flyout heights on Windows 11 (e.g. build 26300) must land
        // right above the Start button instead of leaving a large gap from a hard-coded 724 DIP estimate.
        foreach (var (flyoutTop, flyoutHeight) in new[] { (180.0, 736.0), (348.0, 638.0), (280.0, 706.0) })
        {
            var (tx, ty) = ShellShortcuts.ComputeMeasuredStartMenuPosition(
                windowLeft: 0, windowTop: 0,
                flyoutLeft: 544, flyoutTop: flyoutTop, flyoutWidth: 832, flyoutHeight: flyoutHeight,
                workLeft: 0, workTop: 0, workRight: 1920, workBottom: 1080,
                anchorLeft: 628, anchorTop: 1035, anchorRight: 666, anchorBottom: 1073,
                edge: OpenTaskbar.Models.DockEdge.Bottom, dpiScale: 1.0);
            var resolvedLeft = tx + 544;
            var resolvedBottom = ty + flyoutTop + flyoutHeight;
            Check(resolvedLeft == 620,
                $"Measured Start flyout left must align above Start button (got {resolvedLeft}).");
            Check(resolvedBottom == 1009,
                $"Measured Start flyout bottom must sit 26px above anchorTop=1035 for height={flyoutHeight} (got {resolvedBottom}).");
        }

        // Bottom-edge measured Start menu at 150% DPI: anchorLeft is already in physical pixels and must not
        // be multiplied by dpiScale a second time.
        {
            var (tx, ty) = ShellShortcuts.ComputeMeasuredStartMenuPosition(
                windowLeft: 0, windowTop: 0,
                flyoutLeft: 816, flyoutTop: 300, flyoutWidth: 1248, flyoutHeight: 1050,
                workLeft: 0, workTop: 0, workRight: 2880, workBottom: 1620,
                anchorLeft: 942, anchorTop: 1552, anchorRight: 999, anchorBottom: 1609,
                edge: OpenTaskbar.Models.DockEdge.Bottom, dpiScale: 1.5);
            var resolvedLeft = tx + 816;
            var resolvedBottom = ty + 300 + 1050;
            Check(resolvedLeft == 930,
                $"High-DPI measured Start flyout must not double-scale anchorLeft (got {resolvedLeft}).");
            Check(resolvedBottom == 1513,
                $"High-DPI measured Start flyout bottom must sit 39px above anchorTop=1552 (got {resolvedBottom}).");
        }

        // Bottom-edge Search: left-align with the Search button so wide panels open directly above the button
        // instead of shifting hundreds of pixels left of the dock.
        {
            var (sx, sy) = ShellShortcuts.ComputeSearchPosition(
                858, 938, 0, 0, 1920, 1080,
                anchorLeft: 666, anchorTop: 1035, anchorRight: 704, anchorBottom: 1073,
                edge: OpenTaskbar.Models.DockEdge.Bottom, dpiScale: 1.0);
            Check(sx == 658, $"Bottom-edge Search must left-align above the Search button (got {sx}).");
            Check(sy + 938 == 1027, $"Bottom-edge Search bottom must sit 8px above anchorTop=1035 (got {sy + 938}).");

            // Near the right edge of the work area, Search must clamp inside the monitor work area.
            var (clampedX, _) = ShellShortcuts.ComputeSearchPosition(
                858, 938, 0, 0, 1920, 1080,
                anchorLeft: 1700, anchorTop: 1035, anchorRight: 1738, anchorBottom: 1073,
                edge: OpenTaskbar.Models.DockEdge.Bottom, dpiScale: 1.0);
            Check(clampedX + 858 <= 1912, $"Right-side Search must clamp within work area (got {clampedX}).");
        }

        // Start menu + Phone Link companion panel: whether UIA seeds on the Phone Link panel or the main Start
        // panel, ExpandStartFlyoutBounds must merge both side-by-side panels so the main Start menu is never pushed
        // off the left edge and Phone Link never clips off the right edge.
        {
            var root = new System.Windows.Rect(0, 0, 1920, 1080);
            var mainPanel = new System.Windows.Rect(460, 260, 642, 720);
            var phoneLinkPanel = new System.Windows.Rect(1118, 260, 320, 720);
            var footer = new System.Windows.Rect(472, 956, 618, 44);
            var panels = new[] { mainPanel, phoneLinkPanel };
            var visible = new[] { phoneLinkPanel, footer }; // Simulate mainPanel temporarily offscreen.

            // Seeded on Phone Link: must expand left to include mainPanel and footer.
            var mergedFromPhone = ShellShortcuts.ExpandStartFlyoutBounds(root, phoneLinkPanel, visible, panels);
            Check(mergedFromPhone.Left == 460 && mergedFromPhone.Right == 1438 && mergedFromPhone.Bottom == 1000,
                $"Start+PhoneLink merged bounds from Phone Link seed must span both panels and footer (got {mergedFromPhone}).");

            // Repositioning using merged bounds must align main Start panel's left edge above Start button.
            var (tx, _) = ShellShortcuts.ComputeMeasuredStartMenuPosition(
                windowLeft: 0, windowTop: 0,
                flyoutLeft: mergedFromPhone.Left, flyoutTop: mergedFromPhone.Top,
                flyoutWidth: mergedFromPhone.Width, flyoutHeight: mergedFromPhone.Height,
                workLeft: 0, workTop: 0, workRight: 1920, workBottom: 1080,
                anchorLeft: 520, anchorTop: 1035, anchorRight: 558, anchorBottom: 1073,
                edge: OpenTaskbar.Models.DockEdge.Bottom, dpiScale: 1.0);
            Check(tx + mainPanel.Left == 512,
                $"Main Start panel left edge must align above Start button when Phone Link is present (got {tx + mainPanel.Left}).");

            // Seeded on mainPanel near right edge: Phone Link must also stay inside the work area.
            var mergedFromMain = ShellShortcuts.ExpandStartFlyoutBounds(root, mainPanel, visible, panels);
            var (clampedTx, _) = ShellShortcuts.ComputeMeasuredStartMenuPosition(
                windowLeft: 0, windowTop: 0,
                flyoutLeft: mergedFromMain.Left, flyoutTop: mergedFromMain.Top,
                flyoutWidth: mergedFromMain.Width, flyoutHeight: mergedFromMain.Height,
                workLeft: 0, workTop: 0, workRight: 1920, workBottom: 1080,
                anchorLeft: 1400, anchorTop: 1035, anchorRight: 1438, anchorBottom: 1073,
                edge: OpenTaskbar.Models.DockEdge.Bottom, dpiScale: 1.0);
            Check(clampedTx + phoneLinkPanel.Right <= 1912,
                $"Phone Link companion panel must clamp inside work area near right edge (got {clampedTx + phoneLinkPanel.Right}).");
        }

        return checks;
    }
}
