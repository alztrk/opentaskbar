using System.Globalization;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public enum CountdownState { NotConfigured, Ready, Running, Paused, Finished }

public sealed class CountdownDockWidget(TimeProvider timeProvider) : DockWidgetItem(DockWidgetService.CountdownId, UiText.Get("WidgetCountdown")), ITimedDockWidget
{
    public const int MinimumMinutes = 1;
    public const int MaximumMinutes = 1440;
    private string _minutesText = string.Empty;
    private TimeSpan _totalDuration;
    private TimeSpan _remaining;
    private long _startedAt;

    public CountdownState State { get; private set; }
    public bool IsRunning => State == CountdownState.Running;
    public bool CanToggle => State != CountdownState.NotConfigured;
    public bool CanEditDuration => State is CountdownState.NotConfigured or CountdownState.Ready or CountdownState.Finished;
    public bool HasValidationError => State == CountdownState.NotConfigured && !string.IsNullOrWhiteSpace(MinutesText);
    public string DurationHelpText => UiText.Format("CountdownEditingHelp", MinimumMinutes, MaximumMinutes);
    public TimeSpan Remaining => IsRunning
        ? TimeSpan.FromTicks(Math.Max(0, (_remaining - timeProvider.GetElapsedTime(_startedAt)).Ticks))
        : _remaining;

    public double ProgressPercent => _totalDuration.TotalSeconds > 0
        ? Math.Clamp(Remaining.TotalSeconds / _totalDuration.TotalSeconds * 100.0, 0, 100)
        : 0;

    public string StatusText => State switch
    {
        CountdownState.NotConfigured when HasValidationError => UiText.Format("CountdownDurationError", MinimumMinutes, MaximumMinutes),
        CountdownState.NotConfigured => UiText.Get("CountdownChooseDuration"),
        CountdownState.Finished => UiText.Get("CountdownFinished"),
        _ => string.Empty
    };

    public string MinutesText
    {
        get => _minutesText;
        set
        {
            if (!CanEditDuration || _minutesText == value) return;
            _minutesText = value;
            NotifyChanged(nameof(MinutesText));
            Reset();
            NotifyChanged(nameof(HasValidationError));
        }
    }

    public void SetPresetMinutes(int minutes)
    {
        var clamped = Math.Clamp(minutes, MinimumMinutes, MaximumMinutes);
        _minutesText = clamped.ToString(CultureInfo.InvariantCulture);
        NotifyChanged(nameof(MinutesText));
        _totalDuration = TimeSpan.FromMinutes(clamped);
        _remaining = _totalDuration;
        SetState(CountdownState.Ready);
        NotifyChanged(nameof(HasValidationError));
        Refresh();
    }

    public void Toggle()
    {
        if (!CanToggle) return;
        if (IsRunning)
        {
            _remaining = Remaining;
            SetState(_remaining == TimeSpan.Zero ? CountdownState.Finished : CountdownState.Paused);
        }
        else
        {
            if (State == CountdownState.Finished)
            {
                _totalDuration = ReadDuration();
                _remaining = _totalDuration;
            }
            _startedAt = timeProvider.GetTimestamp();
            SetState(CountdownState.Running);
        }
        Refresh();
    }

    public void Reset()
    {
        _totalDuration = ReadDuration();
        _remaining = _totalDuration;
        SetState(_remaining > TimeSpan.Zero ? CountdownState.Ready : CountdownState.NotConfigured);
        Refresh();
    }

    public void Refresh()
    {
        var remaining = Remaining;
        if (IsRunning && remaining == TimeSpan.Zero)
        {
            _remaining = TimeSpan.Zero;
            SetState(CountdownState.Finished);
        }
        remaining = TimeSpan.FromSeconds(Math.Ceiling(remaining.TotalSeconds));
        Value = State == CountdownState.NotConfigured ? string.Empty
            : $"{(long)remaining.TotalHours:00}:{remaining.Minutes:00}:{remaining.Seconds:00}";
        NotifyChanged(nameof(ProgressPercent));
        NotifyChanged(nameof(StatusText));
    }

    private TimeSpan ReadDuration() => int.TryParse(MinutesText.Trim(), NumberStyles.None, CultureInfo.InvariantCulture, out var minutes)
        && minutes is >= MinimumMinutes and <= MaximumMinutes ? TimeSpan.FromMinutes(minutes) : TimeSpan.Zero;

    private void SetState(CountdownState state)
    {
        if (State == state) return;
        State = state;
        NotifyChanged(nameof(State));
        NotifyChanged(nameof(IsRunning));
        NotifyChanged(nameof(CanToggle));
        NotifyChanged(nameof(CanEditDuration));
        NotifyChanged(nameof(HasValidationError));
    }
}
