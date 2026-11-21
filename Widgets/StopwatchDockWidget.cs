using System.Collections.ObjectModel;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed record StopwatchLapEntry(int Number, string FormattedTime);

public sealed class StopwatchDockWidget(TimeProvider timeProvider) : DockWidgetItem(DockWidgetService.StopwatchId, UiText.Get("WidgetStopwatch")), ITimedDockWidget
{
    private TimeSpan _elapsed;
    private long _startedAt;
    private string _detailedValue = "00:00.00";

    public bool IsRunning { get; private set; }
    public bool CanToggle => true;
    public TimeSpan Elapsed => _elapsed + (IsRunning ? timeProvider.GetElapsedTime(_startedAt) : TimeSpan.Zero);
    public ObservableCollection<StopwatchLapEntry> Laps { get; } = [];
    public bool HasLaps => Laps.Count > 0;

    public string DetailedValue
    {
        get => _detailedValue;
        private set
        {
            if (_detailedValue == value) return;
            _detailedValue = value;
            NotifyChanged(nameof(DetailedValue));
        }
    }

    public void Toggle()
    {
        if (IsRunning)
            _elapsed = Elapsed;
        else
            _startedAt = timeProvider.GetTimestamp();
        IsRunning = !IsRunning;
        NotifyChanged(nameof(IsRunning));
        Refresh();
    }

    public void RecordLap()
    {
        var elapsed = Elapsed;
        var formatted = elapsed.TotalHours >= 1
            ? $"{(long)elapsed.TotalHours:00}:{elapsed.Minutes:00}:{elapsed.Seconds:00}.{elapsed.Milliseconds / 10:00}"
            : $"{elapsed.Minutes:00}:{elapsed.Seconds:00}.{elapsed.Milliseconds / 10:00}";
        Laps.Insert(0, new StopwatchLapEntry(Laps.Count + 1, formatted));
        NotifyChanged(nameof(HasLaps));
    }

    public void Reset()
    {
        IsRunning = false;
        _elapsed = TimeSpan.Zero;
        Laps.Clear();
        NotifyChanged(nameof(HasLaps));
        NotifyChanged(nameof(IsRunning));
        Refresh();
    }

    public void Refresh()
    {
        var elapsed = Elapsed;
        Value = $"{(long)elapsed.TotalHours:00}:{elapsed.Minutes:00}:{elapsed.Seconds:00}";
        DetailedValue = elapsed.TotalHours >= 1
            ? $"{(long)elapsed.TotalHours:00}:{elapsed.Minutes:00}:{elapsed.Seconds:00}"
            : $"{elapsed.Minutes:00}:{elapsed.Seconds:00}.{elapsed.Milliseconds / 10:00}";
    }
}
