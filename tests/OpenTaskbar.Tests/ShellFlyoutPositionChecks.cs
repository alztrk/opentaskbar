using System.IO;
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

        return checks;
    }
}
