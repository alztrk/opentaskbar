using System.IO;
using System.Windows.Threading;

namespace OpenTaskbar.Services;

/// <summary>Tracks settings saved by the dock from a separate Fjord process.</summary>
internal sealed class FjordFilesAvailability : IDisposable
{
    private readonly DockSettingsStore _store;
    private readonly DispatcherTimer _timer;
    private DateTime? _lastWrite;

    internal bool Enabled { get; private set; }
    internal event Action<bool>? Changed;

    internal FjordFilesAvailability(DockSettingsStore store, bool enabled)
    {
        _store = store;
        Enabled = enabled;
        _timer = new DispatcherTimer(DispatcherPriority.Background)
        {
            Interval = TimeSpan.FromMilliseconds(750)
        };
        _timer.Tick += (_, _) => Refresh();
        _timer.Start();
    }

    internal bool Refresh()
    {
        DateTime lastWrite;
        try { lastWrite = File.GetLastWriteTimeUtc(_store.SettingsPath); }
        catch (IOException) { return Enabled; }
        catch (UnauthorizedAccessException) { return Enabled; }

        if (_lastWrite == lastWrite) return Enabled;
        var (settings, error) = _store.Load();
        if (error is not null) return Enabled;
        _lastWrite = lastWrite;
        if (Enabled == settings.FjordFilesEnabled) return Enabled;
        Enabled = settings.FjordFilesEnabled;
        Changed?.Invoke(Enabled);
        return Enabled;
    }

    public void Dispose() => _timer.Stop();
}
