namespace OpenTaskbar.Services;

/// <summary>
/// Remembers where Windows placed its own shell flyouts before the dock anchored them to itself, so shutting the
/// dock down can put them back. Handles are checked against their process before restoring because Windows
/// recycles HWNDs, and a generation counter lets in-flight reposition loops notice a shutdown and stop moving.
/// </summary>
internal sealed class ShellFlyoutPositionRegistry(
    Func<nint, uint> processIdOf,
    Func<nint, bool> windowExists,
    Action<nint, int, int> move,
    Action<IEnumerable<FlyoutPosition>>? persist = null,
    Action? clearPersisted = null)
{
    private readonly object _gate = new();
    private readonly Dictionary<nint, FlyoutPosition> _positions = [];
    private int _generation;

    /// <summary>Changes when the flyouts are restored; reposition loops capture it before they start moving windows.</summary>
    public int Generation => Volatile.Read(ref _generation);

    /// <summary>Stores the first position seen for a handle, ignoring windows that have since been recycled.</summary>
    public void Remember(nint handle, int left, int top)
    {
        if (handle == 0) return;
        lock (_gate)
        {
            PruneDeadHandles();
            if (!_positions.ContainsKey(handle))
            {
                _positions[handle] = new FlyoutPosition(handle, processIdOf(handle), left, top);
                persist?.Invoke(_positions.Values.ToArray());
            }
        }
    }

    /// <summary>Stops any pending moves and returns every still-valid window to its original position.</summary>
    public void Restore()
    {
        Interlocked.Increment(ref _generation);
        List<FlyoutPosition> positions;
        lock (_gate)
        {
            positions = [.. _positions.Values];
            _positions.Clear();
        }

        var ownedBySomeoneElse = false;
        foreach (var position in positions)
        {
            if (!windowExists(position.Handle)) continue;
            if (processIdOf(position.Handle) != position.ProcessId)
            {
                // A recycled handle must not move an unrelated window, and this journal may still be useful
                // to a recovery path that runs later in the same session.
                ownedBySomeoneElse = true;
                continue;
            }
            move(position.Handle, position.Left, position.Top);
        }
        if (!ownedBySomeoneElse) clearPersisted?.Invoke();
    }

    private void PruneDeadHandles()
    {
        foreach (var handle in _positions.Keys.Where(handle =>
                     !windowExists(handle) || processIdOf(handle) != _positions[handle].ProcessId).ToList())
        {
            _positions.Remove(handle);
        }
    }
}
