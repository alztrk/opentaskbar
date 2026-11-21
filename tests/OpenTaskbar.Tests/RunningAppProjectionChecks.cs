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

        return checks;
    }
}
