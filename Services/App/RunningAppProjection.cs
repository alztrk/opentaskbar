using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>One app button's worth of tracked windows, in enumeration (Z-order) order.</summary>
public sealed record RunningAppCandidate(
    string ExecutablePath,
    IReadOnlyList<TrackedWindow> Windows,
    bool HasNotification,
    bool IsForeground)
{
    /// <summary>The topmost window decides the identity icon; ApplicationFrameHost hosts several Store apps.</summary>
    public nint IconWindow => Windows[0].Handle;

    public int WindowCount => Windows.Count;
}

/// <summary>
/// The single gate that turns tracked windows into running-app buttons. The primary dock, secondary docks,
/// notification badges and running indicators all consume this projection, so a background window can never
/// be a normal app button in one place and hidden in another.
/// </summary>
internal static class RunningAppProjection
{
    internal static IReadOnlyList<RunningAppCandidate> Project(
        IReadOnlyList<TrackedWindow> windows,
        ISet<string> excludedPaths,
        nint foreground,
        bool notificationBadgesEnabled,
        Func<string, bool> fileExists)
    {
        var groups = new Dictionary<string, List<TrackedWindow>>(StringComparer.OrdinalIgnoreCase);
        var order = new List<string>();
        for (var i = 0; i < windows.Count; i++)
        {
            var window = windows[i];
            var path = window.ExecutablePath;
            if (string.IsNullOrEmpty(path) || excludedPaths.Contains(path) || !fileExists(path)) continue;
            if (!groups.TryGetValue(path, out var group))
            {
                group = [];
                groups.Add(path, group);
                order.Add(path);
            }
            group.Add(window);
        }

        var candidates = new List<RunningAppCandidate>(order.Count);
        foreach (var path in order)
        {
            var group = groups[path];
            candidates.Add(new RunningAppCandidate(
                path,
                group,
                notificationBadgesEnabled && group.Any(window => window.HasNotification),
                group.Any(window => window.Handle == foreground)));
        }
        return candidates;
    }
}
