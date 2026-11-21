using System.ComponentModel;
using System.Media;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// V19 Focus timer widget: compact target icon + tabular remaining time + play/pause on the dock;
/// distinct Focus flyout with prominent remaining time, horizontal progress bar, 15/25/50 min presets,
/// session count, reset, and start/pause.
/// </summary>
public partial class FocusWidget : DockWidget
{
    private enum Phase { Focus, Break }

    private static readonly int[] FocusPresets = [15, 25, 50];

    private Phase _phase = Phase.Focus;
    private int _session = 1;
    private int _completedSessions;
    private int _activePresetMinutes;
    private TimeSpan _remaining;
    private TimeSpan _lastFullLength;
    private long? _startedAt;
    private readonly TimeProvider _clock;
    private readonly DockFlyout _flyout = new();
    private bool _initialized;

    private int _renderedSeconds = -1;
    private Phase? _renderedPhase;
    private bool? _renderedRunning;
    private AppLanguage? _renderedLanguage;

    private Border? _flyoutContainer;
    private TextBlock? _flyoutTimeText;
    private Border? _flyoutProgressFill;
    private ColumnDefinition? _flyoutProgressFillCol;
    private ColumnDefinition? _flyoutProgressEmptyCol;
    private TextBlock? _flyoutSessionCount;
    private Button? _flyoutToggleButton;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 50;
    public override double FullWidthEstimate => 118;
    public override double CompactWidthEstimate => 96;

    public FocusWidget() : this(TimeProvider.System) { }

    public FocusWidget(TimeProvider clock)
    {
        _clock = clock;
        InitializeComponent();
        _flyout.Closed += (_, _) =>
        {
            _flyoutContainer = null;
            _flyoutTimeText = null;
            _flyoutProgressFill = null;
            _flyoutProgressFillCol = null;
            _flyoutProgressEmptyCol = null;
            _flyoutSessionCount = null;
            _flyoutToggleButton = null;
        };
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        Card.Padding = compact ? new Thickness(7, 0, 6, 0) : new Thickness(10, 0, 8, 0);
        FocusIcon.Margin = compact ? new Thickness(0, 0, 5, 0) : new Thickness(0, 0, 7, 0);
    }

    public override UIElement? BuildHoverFlyoutContent() => null;

    private TimeSpan PhaseLength
    {
        get
        {
            if (_phase == Phase.Focus)
            {
                var mins = _activePresetMinutes > 0 ? _activePresetMinutes : Settings.FocusMinutes;
                return TimeSpan.FromMinutes(Math.Max(1, mins));
            }
            return TimeSpan.FromMinutes(Math.Max(1, Settings.FocusBreakMinutes));
        }
    }

    private bool IsRunning => _startedAt is not null;
    private TimeSpan CurrentRemaining => _startedAt is { } startedAt
        ? _remaining - _clock.GetElapsedTime(startedAt)
        : _remaining;

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        if (!_initialized)
        {
            _activePresetMinutes = Settings.FocusMinutes;
            _remaining = PhaseLength;
            _lastFullLength = _remaining;
            _initialized = true;
        }
        Render();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        if (IsRunning)
        {
            _remaining = TimeSpan.FromTicks(Math.Max(0, CurrentRemaining.Ticks));
            _startedAt = null;
        }
        Card.BeginAnimation(OpacityProperty, null);
    }

    protected override void OnDisposed()
    {
        _flyout.Close();
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (IsActive) Render();
    }

    protected override void OnSettingsChanged()
    {
        _session = Math.Min(_session, Settings.FocusSessions);
        if (!_initialized || (!IsRunning && _remaining == _lastFullLength))
        {
            _activePresetMinutes = Settings.FocusMinutes;
            _remaining = PhaseLength;
            _initialized = true;
        }

        _lastFullLength = PhaseLength;
        Render();
    }

    public override bool SupportsStaticPreview => true;
    protected override void OnRenderPreview()
    {
        if (!_initialized)
        {
            _activePresetMinutes = Settings.FocusMinutes;
            _remaining = PhaseLength;
            _lastFullLength = _remaining;
            _initialized = true;
        }
        Render();
    }

    protected override void OnTick(DateTime now)
    {
        if (!IsRunning) return;
        if (CurrentRemaining <= TimeSpan.Zero) CompletePhase();
        Render();
    }

    private void CompletePhase()
    {
        SystemSounds.Asterisk.Play();
        Pulse();
        if (_phase == Phase.Focus)
        {
            _completedSessions++;
            StartPhase(Phase.Break, run: true);
        }
        else
        {
            _session = _session >= Settings.FocusSessions ? 1 : _session + 1;
            StartPhase(Phase.Focus, run: false);
        }
    }

    private void StartPhase(Phase phase, bool run)
    {
        _phase = phase;
        _remaining = PhaseLength;
        _lastFullLength = _remaining;
        _startedAt = run ? _clock.GetTimestamp() : null;
    }

    private void Card_MouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is DependencyObject dep && IsDescendantOf(dep, PlayButton))
            return;

        _flyout.Toggle(Card, BuildFocusFlyout, animate: true, focusable: false);
        e.Handled = true;
    }

    internal UIElement BuildFocusFlyout()
    {
        var container = new Border { Width = 316 };
        _flyoutContainer = container;
        PopulateFocusFlyout(container);
        return container;
    }

    private void PopulateFocusFlyout(Border container)
    {
        var root = new StackPanel();

        // Header: Focus/Break label + completed session count
        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 10) };
        var sessionCount = new TextBlock
        {
            Text = UiText.Format("FocusSessionsCount", _completedSessions),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        sessionCount.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(sessionCount, Dock.Right);
        header.Children.Add(sessionCount);

        var title = new TextBlock
        {
            Text = UiText.Get(_phase == Phase.Focus ? "Focus" : "Break"),
            FontSize = 12.5,
            FontWeight = FontWeights.Bold,
            VerticalAlignment = VerticalAlignment.Center
        };
        title.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        header.Children.Add(title);
        root.Children.Add(header);
        _flyoutSessionCount = sessionCount;

        // Prominent remaining time
        var timeText = new TextBlock
        {
            Text = FormatRemaining(),
            FontSize = 48,
            FontWeight = FontWeights.Bold,
            Margin = new Thickness(0, 2, 0, 12),
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        timeText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        root.Children.Add(timeText);
        _flyoutTimeText = timeText;

        // Horizontal progress track
        var pct = RemainingFraction();
        var track = new Border { Height = 7, CornerRadius = new CornerRadius(3.5), Margin = new Thickness(0, 0, 0, 14), ClipToBounds = true };
        track.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");
        var trackGrid = new Grid();
        var fillCol = new ColumnDefinition { Width = new GridLength(pct, GridUnitType.Star) };
        var emptyCol = new ColumnDefinition { Width = new GridLength(Math.Max(0, 1 - pct), GridUnitType.Star) };
        trackGrid.ColumnDefinitions.Add(fillCol);
        trackGrid.ColumnDefinitions.Add(emptyCol);
        var fill = new Border { CornerRadius = new CornerRadius(3.5) };
        fill.SetResourceReference(Border.BackgroundProperty, "WidgetBlue");
        Grid.SetColumn(fill, 0);
        trackGrid.Children.Add(fill);
        track.Child = trackGrid;
        root.Children.Add(track);
        _flyoutProgressFill = fill;
        _flyoutProgressFillCol = fillCol;
        _flyoutProgressEmptyCol = emptyCol;

        // 15 / 25 / 50 minute preset row
        var presetsGrid = new UniformGrid { Columns = FocusPresets.Length, Margin = new Thickness(0, 0, 0, 10) };
        var currentMins = (int)Math.Round(PhaseLength.TotalMinutes);
        for (var i = 0; i < FocusPresets.Length; i++)
        {
            var mins = FocusPresets[i];
            var isSelected = _phase == Phase.Focus && currentMins == mins;
            var presetBtn = new Button
            {
                Content = UiText.Format("MinutesShort", mins),
                Margin = new Thickness(i == 0 ? 0 : 4, 0, i == FocusPresets.Length - 1 ? 0 : 4, 0)
            };
            presetBtn.SetResourceReference(StyleProperty, isSelected ? "WidgetFlyoutPrimaryButton" : "WidgetFlyoutActionButton");
            presetBtn.Click += (_, _) =>
            {
                _phase = Phase.Focus;
                _activePresetMinutes = mins;
                _remaining = TimeSpan.FromMinutes(mins);
                _lastFullLength = _remaining;
                _startedAt = null;
                Render();
                if (_flyoutContainer is not null) PopulateFocusFlyout(_flyoutContainer);
            };
            presetsGrid.Children.Add(presetBtn);
        }
        root.Children.Add(presetsGrid);

        // Reset + Start/Pause action row
        var actionsGrid = new UniformGrid { Columns = 2 };
        var resetBtn = new Button
        {
            Content = UiText.Get("FocusReset"),
            Margin = new Thickness(0, 0, 4, 0)
        };
        resetBtn.SetResourceReference(StyleProperty, "WidgetFlyoutActionButton");
        resetBtn.Click += Reset_Click;

        var toggleBtn = new Button
        {
            Content = UiText.Get(IsRunning ? "FocusPause" : "FocusStart"),
            Margin = new Thickness(4, 0, 0, 0)
        };
        toggleBtn.SetResourceReference(StyleProperty, "WidgetFlyoutPrimaryButton");
        toggleBtn.Click += Toggle_Click;
        _flyoutToggleButton = toggleBtn;

        actionsGrid.Children.Add(resetBtn);
        actionsGrid.Children.Add(toggleBtn);
        root.Children.Add(actionsGrid);

        container.Child = root;
    }

    private void Toggle_Click(object sender, RoutedEventArgs e)
    {
        if (IsRunning)
        {
            _remaining = TimeSpan.FromTicks(Math.Max(0, CurrentRemaining.Ticks));
            _startedAt = null;
        }
        else
        {
            if (_remaining <= TimeSpan.Zero) _remaining = PhaseLength;
            _startedAt = _clock.GetTimestamp();
        }

        Render();
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        _session = 1;
        StartPhase(Phase.Focus, run: false);
        Render();
    }

    private string FormatRemaining()
    {
        var elapsed = CurrentRemaining;
        var remaining = elapsed < TimeSpan.Zero ? TimeSpan.Zero : elapsed;
        var seconds = (int)Math.Ceiling(remaining.TotalSeconds);
        return $"{seconds / 60:00}:{seconds % 60:00}";
    }

    private double RemainingFraction()
    {
        var total = Math.Max(1.0, PhaseLength.TotalSeconds);
        var rem = Math.Clamp(CurrentRemaining.TotalSeconds, 0, total);
        return rem / total;
    }

    private void Render()
    {
        var elapsed = CurrentRemaining;
        var remaining = elapsed < TimeSpan.Zero ? TimeSpan.Zero : elapsed;
        var seconds = (int)Math.Ceiling(remaining.TotalSeconds);
        if (_renderedSeconds != seconds)
        {
            _renderedSeconds = seconds;
            var formatted = $"{seconds / 60:00}:{seconds % 60:00}";
            RemainingText.Text = formatted;
            if (_flyoutTimeText is not null) _flyoutTimeText.Text = formatted;
        }

        if (_flyoutProgressFillCol is not null && _flyoutProgressEmptyCol is not null)
        {
            var pct = RemainingFraction();
            _flyoutProgressFillCol.Width = new GridLength(pct, GridUnitType.Star);
            _flyoutProgressEmptyCol.Width = new GridLength(Math.Max(0, 1 - pct), GridUnitType.Star);
        }

        if (_flyoutSessionCount is not null)
        {
            _flyoutSessionCount.Text = UiText.Format("FocusSessionsCount", _completedSessions);
        }

        var lang = UiText.Current.Language;
        var phaseChanged = _renderedPhase != _phase;
        var runningChanged = _renderedRunning != IsRunning;
        var langChanged = _renderedLanguage != lang;

        if (runningChanged)
        {
            ToggleGlyph.Content = FindResource(IsRunning ? "IconPauseFill" : "IconPlayFill");
            FocusIcon.SetResourceReference(ForegroundProperty, IsRunning ? "WidgetSelectedForeground" : "TextSecondary");
            PlayButton.SetResourceReference(BackgroundProperty, IsRunning ? "WidgetSelectedFill" : "Transparent");
            PlayButton.SetResourceReference(ForegroundProperty, IsRunning ? "WidgetSelectedForeground" : "TextPrimary");
        }

        if (runningChanged || langChanged)
        {
            var playLabel = UiText.Get(IsRunning ? "FocusPause" : "FocusStart");
            PlayButton.ToolTip = playLabel;
            AutomationProperties.SetName(PlayButton, playLabel);
            if (_flyoutToggleButton is not null) _flyoutToggleButton.Content = playLabel;
        }

        Card.ToolTip = $"{UiText.Get(_phase == Phase.Focus ? "Focus" : "Break")} · {RemainingText.Text}";

        _renderedPhase = _phase;
        _renderedRunning = IsRunning;
        _renderedLanguage = lang;
    }

    private void Pulse()
    {
        if (!Settings.AnimationsEnabled || !SystemParameters.ClientAreaAnimation) return;
        var pulse = new DoubleAnimation(1, 0.35, TimeSpan.FromMilliseconds(260))
        {
            AutoReverse = true,
            RepeatBehavior = new RepeatBehavior(3)
        };
        pulse.Completed += (_, _) => Card.BeginAnimation(OpacityProperty, null);
        Card.BeginAnimation(OpacityProperty, pulse);
    }

    private static bool IsDescendantOf(DependencyObject node, DependencyObject target)
    {
        var current = node;
        while (current is not null)
        {
            if (ReferenceEquals(current, target)) return true;
            current = current is Visual || current is System.Windows.Media.Media3D.Visual3D
                ? VisualTreeHelper.GetParent(current)
                : LogicalTreeHelper.GetParent(current);
        }
        return false;
    }
}
