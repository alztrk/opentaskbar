using System.Windows.Threading;

namespace OpenTaskbar.Services;

/// <summary>
/// Applies a temporary pure-black, glow-free look when Night Light is running or the battery drops
/// below the configured threshold. The change lives in ThemeManager only: saved settings and the
/// selected theme pack are never modified, so switching the trigger off restores the user's look.
/// </summary>
internal sealed class DisplayCareController : IDisposable
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromSeconds(30);

    private readonly ApplicationPreferences _preferences;
    private readonly DispatcherTimer _timer;
    private bool _forced;

    public DisplayCareController(ApplicationPreferences preferences)
    {
        _preferences = preferences;
        _timer = new DispatcherTimer(DispatcherPriority.Background) { Interval = PollInterval };
        _timer.Tick += RefreshTrigger;
        Refresh();
    }

    /// <summary>Re-evaluates the trigger configuration after a settings save.</summary>
    public void Refresh()
    {
        var settings = _preferences.Current;
        if (!settings.OledAutoTriggerEnabled)
        {
            _timer.Stop();
            Force(false);
            return;
        }
        if (!_timer.IsEnabled) _timer.Start();
        RefreshTrigger(this, EventArgs.Empty);
    }

    private void RefreshTrigger(object? sender, EventArgs e)
    {
        var settings = _preferences.Current;
        if (!settings.OledAutoTriggerEnabled)
        {
            Force(false);
            return;
        }

        var nightLight = settings.OledAutoOnNightLight && NightLightControl.Read() == true;
        var battery = settings.OledLowBatteryThreshold > 0
            && SystemStats.SampleBattery() is { Present: true, Charging: false } status
            && status.Percent <= settings.OledLowBatteryThreshold;
        Force(nightLight || battery);
    }

    private void Force(bool enabled)
    {
        if (_forced == enabled) return;
        _forced = enabled;
        ThemeManager.SetForcedDisplayCare(enabled);
    }

    public void Dispose()
    {
        _timer.Stop();
        _timer.Tick -= RefreshTrigger;
        Force(false);
    }
}
