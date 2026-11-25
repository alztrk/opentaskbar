using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>Converts a drop position in the visible dock into the saved item order.</summary>
internal static class DockItemOrdering
{
    /// <summary>Windows-native shell buttons that are always pinned to the left edge of the dock.</summary>
    internal static readonly string[] PinnedLeadTokens = ["start", "search", "fjord-files", "taskview"];

    /// <summary>
    /// The background-apps button and the Recycle Bin are always pinned to the right edge of the dock,
    /// with the bin occupying the rightmost slot.
    /// </summary>
    internal static readonly string[] PinnedTrailTokens = ["tray", "bin"];

    private static readonly string[] PinnedTokens = [.. PinnedLeadTokens, .. PinnedTrailTokens];

    internal static bool IsPinned(string token) =>
        PinnedTokens.Contains(token, StringComparer.OrdinalIgnoreCase);

    /// <summary>
    /// Insertion index for a pointer position along the dock: the number of visible slots (dragged item included)
    /// whose center lies before the pointer. This is the index <see cref="MoveVisibleItem"/> expects.
    /// </summary>
    internal static int InsertionIndex(
        IReadOnlyList<(double Start, double Size)> slots,
        double pointer,
        int currentIndex = -1,
        double hysteresis = 0)
    {
        if (currentIndex < 0 || currentIndex > slots.Count)
        {
            var initialIndex = 0;
            foreach (var (start, size) in slots)
            {
                if (pointer > start + size / 2) initialIndex++;
                else break;
            }
            return initialIndex;
        }

        var index = currentIndex;
        while (index < slots.Count && pointer > slots[index].Start + slots[index].Size / 2 + hysteresis)
            index++;
        while (index > 0 && pointer <= slots[index - 1].Start + slots[index - 1].Size / 2 - hysteresis)
            index--;
        return index;
    }

    /// <summary>How far an item slides aside while another is dragged, opening the landing slot.</summary>
    internal static double SlideFor(int item, int dragged, int insertionIndex, double draggedSize)
    {
        if (item == dragged) return 0;
        var destination = insertionIndex > dragged ? insertionIndex - 1 : insertionIndex;
        if (item > dragged && item <= destination) return -draggedSize;
        if (item < dragged && item >= destination) return draggedSize;
        return 0;
    }

    /// <summary>
    /// Rewrites a persisted order so the pinned shell groups always lead and trail the strip. Hidden buttons are
    /// omitted; every other item keeps its saved position between the two groups.
    /// </summary>
    internal static IReadOnlyList<string> ApplyPinnedGroups(IReadOnlyList<string> order, DockSettings settings)
    {
        var lead = new List<string>();
        if (settings.StartPlacement != StartButtonPlacement.Hidden) lead.Add("start");
        if (settings.ShowSearchButton) lead.Add("search");
        if (settings.IsFjordFilesButtonVisible) lead.Add("fjord-files");
        if (settings.ShowTaskViewButton) lead.Add("taskview");

        var result = new List<string>(lead);
        result.AddRange(order.Where(token => !IsPinned(token)));
        if (settings.ShowTrayButton) result.Add("tray");
        if (settings.ShowRecycleBin) result.Add("bin");
        return result;
    }

    internal static IReadOnlyList<string> MoveVisibleItem(
        IReadOnlyList<string> savedOrder,
        IReadOnlyList<string> visibleOrder,
        string movedToken,
        int insertionIndex)
    {
        var comparer = StringComparer.OrdinalIgnoreCase;
        var visible = visibleOrder.Distinct(comparer).ToList();
        var fromIndex = visible.FindIndex(token => comparer.Equals(token, movedToken));
        if (fromIndex < 0) return savedOrder.ToArray();

        // The drop index includes the dragged item, but the insertion list does not.
        var destination = Math.Clamp(insertionIndex, 0, visible.Count);
        if (fromIndex < destination) destination--;

        var source = visible[fromIndex];
        visible.RemoveAt(fromIndex);
        visible.Insert(destination, source);

        // Keep hidden/inactive items in their saved slots instead of counting them as
        // visible drop targets. They should reappear in their previous relative order.
        var visibleSet = new HashSet<string>(visibleOrder, comparer);
        var result = new List<string>(savedOrder.Count + visible.Count);
        var seen = new HashSet<string>(comparer);
        var nextVisible = 0;
        foreach (var token in savedOrder)
        {
            if (!seen.Add(token)) continue;
            if (visibleSet.Contains(token))
            {
                if (nextVisible < visible.Count) result.Add(visible[nextVisible++]);
            }
            else
            {
                result.Add(token);
            }
        }

        while (nextVisible < visible.Count) result.Add(visible[nextVisible++]);
        return result;
    }
}
