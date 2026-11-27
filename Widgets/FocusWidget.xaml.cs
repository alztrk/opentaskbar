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
    internal enum Phase { Focus, Break }

    internal sealed class Session
    {
        public Phase CurrentPhase { get; set; }
        public int Number { get; set; } = 1;
        public int Completed { get; set; }
        public int PresetMinutes { get; set; }
        public TimeSpan Remaining { get; set; }
        public TimeSpan FullLength { get; set; }
        public long? StartedAt { get; set; }
        public bool Initialized { get; set; }
        public int ActiveViews { get; set; }
        public event Action? Changed;
        public void NotifyChanged() => Changed?.Invoke();
    }

    private static readonly int[] FocusPresets = [15, 25, 50];

    private readonly Session _state;
    private readonly TimeProvider _clock;
    private readonly DockFlyout _flyout = new();

    private int _renderedSeconds = -1;
    private Phase? _renderedPhase;
    private bool? _renderedRunning;
    private AppLanguage? _renderedLanguage;

    private Border? _flyoutContainer;
    private TextBlock? _flyoutTimeText;
    private ColumnDefinition? _flyoutProgressFillCol;
    private ColumnDefinition? _flyoutProgressEmptyCol;
    private TextBlock? _flyoutSessionCount;
    private Button? _flyoutToggleButton;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 50;
    public override double FullWidthEstimate => 118;
    public override double CompactWidthEstimate => 78;

    public FocusWidget() : this(TimeProvider.System) { }

    public FocusWidget(TimeProvider clock) : this(clock, new Session()) { }

    internal FocusWidget(TimeProvider clock, Session state)
    {
        _clock = clock;
        _state = state;
        InitializeComponent();
        _flyout.Closed += (_, _) =>
        {
            _flyoutContainer = null;
            _flyoutTimeText = null;
            _flyoutProgressFillCol = null;
            _flyoutProgressEmptyCol = null;
            _flyoutSessionCount = null;
            _flyoutToggleButton = null;
        };
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();
    protected override void OnDynamicIslandChanged(bool dynamicIsland) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        Card.Padding = compact ? new Thickness(8, 0, 8, 0) : new Thickness(10, 0, 8, 0);
        FocusIcon.Margin = compact ? new Thickness(0, 0, 5, 0) : new Thickness(0, 0, 7, 0);
        PlayButton.Visibility = compact && !IsDynamicIsland ? Visibility.Collapsed : Visibility.Visible;
        RaiseContentWidthChanged();
    }

    public override UIElement? BuildHoverFlyoutContent() => null;
    public override UIElement? BuildPreviewFlyoutContent() => BuildFocusFlyout();

    private TimeSpan PhaseLength
    {
        get
        {
            if (_state.CurrentPhase == Phase.Focus)
            {
                var mins = _state.PresetMinutes > 0 ? _state.PresetMinutes : Settings.FocusMinutes;
                return TimeSpan.FromMinutes(Math.Max(1, mins));
            }
            return TimeSpan.FromMinutes(Math.Max(1, Settings.FocusBreakMinutes));
        }
    }

    private bool IsRunning => _state.StartedAt is not null;
    private TimeSpan CurrentRemaining => _state.StartedAt is { } startedAt
        ? _state.Remaining - _clock.GetElapsedTime(startedAt)
        : _state.Remaining;

    protected override void OnActivated()
    {
        _state.ActiveViews++;
        _state.Changed += Render;
        if (!_state.Initialized)
        {
            _state.PresetMinutes = Settings.FocusMinutes;
            _state.Remaining = PhaseLength;
            _state.FullLength = _state.Remaining;
            _state.Initialized = true;
        }
        Render();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        _state.Changed -= Render;
        if (--_state.ActiveViews == 0 && IsRunning)
        {
            _state.Remaining = TimeSpan.FromTicks(Math.Max(0, CurrentRemaining.Ticks));
            _state.StartedAt = null;
        }
        Card.BeginAnimation(OpacityProperty, null);
    }

    protected override void OnLanguageChanged() => Render();

    private int _lastConfiguredFocusMinutes = -1;

    protected override void OnSettingsChanged()
    {
        _state.Number = Math.Min(_state.Number, Settings.FocusSessions);
        var focusMinutesChanged = _lastConfiguredFocusMinutes != Settings.FocusMinutes;
        _lastConfiguredFocusMinutes = Settings.FocusMinutes;
        if (!_state.Initialized || (focusMinutesChanged && !IsRunning && _state.Remaining == _state.FullLength))
        {
            _state.PresetMinutes = Settings.FocusMinutes;
            _state.Remaining = PhaseLength;
            _state.Initialized = true;
        }

        _state.FullLength = PhaseLength;
        Render();
    }

    public override bool SupportsStaticPreview => true;
    protected override void OnRenderPreview()
    {
        if (!_state.Initialized)
        {
            _state.PresetMinutes = Settings.FocusMinutes;
            _state.Remaining = PhaseLength;
            _state.FullLength = _state.Remaining;
            _state.Initialized = true;
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
        if (_state.CurrentPhase == Phase.Focus)
        {
            _state.Completed++;
            StartPhase(Phase.Break, run: true);
        }
        else
        {
            _state.Number = _state.Number >= Settings.FocusSessions ? 1 : _state.Number + 1;
            StartPhase(Phase.Focus, run: false);
        }
        _state.NotifyChanged();
    }

    private void StartPhase(Phase phase, bool run)
    {
        _state.CurrentPhase = phase;
        _state.Remaining = PhaseLength;
        _state.FullLength = _state.Remaining;
        _state.StartedAt = run ? _clock.GetTimestamp() : null;
    }

    private void Card_MouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is DependencyObject dep && IsDescendantOf(dep, PlayButton))
            return;
        if (IsDynamicIsland) return;

        _flyout.Toggle(Card, BuildFocusFlyout, animate: true, focusable: false);
        e.Handled = true;
    }

    internal UIElement BuildFocusFlyout()
    {
        var container = new Border { Width = 296 };
        _flyoutContainer = container;
        PopulateFocusFlyout(container);
        return container;
    }

    private void PopulateFocusFlyout(Border container)
    {
        var root = new StackPanel();

        // Unified header: Focus/Break title on left, completed session count on right.
        var sessionCount = new TextBlock
        {
            Text = UiText.Format("FocusSessionsCount", _state.Completed),
            FontSize = 11,
            VerticalAlignment = VerticalAlignment.Center,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        sessionCount.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        _flyoutSessionCount = sessionCount;
        root.Children.Add(DockFlyout.Header(UiText.Get(_state.CurrentPhase == Phase.Focus ? "Focus" : "Break"), sessionCount, bottomMargin: 6));

        // Restrained tabular timer readout + compact 4px progress bar.
        var timeText = new TextBlock
        {
            Text = FormatRemaining(),
            FontSize = 32,
            FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(0, 0, 0, 10),
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        timeText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        root.Children.Add(timeText);
        _flyoutTimeText = timeText;

        var pct = RemainingFraction();
        var track = new Border { Height = 4, CornerRadius = new CornerRadius(2), Margin = new Thickness(0, 0, 0, 12), ClipToBounds = true };
        track.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");
        var trackGrid = new Grid();
        var fillCol = new ColumnDefinition { Width = new GridLength(pct, GridUnitType.Star) };
        var emptyCol = new ColumnDefinition { Width = new GridLength(Math.Max(0, 1 - pct), GridUnitType.Star) };
        trackGrid.ColumnDefinitions.Add(fillCol);
        trackGrid.ColumnDefinitions.Add(emptyCol);
        var fill = new Border { CornerRadius = new CornerRadius(2) };
        fill.SetResourceReference(Border.BackgroundProperty, "WidgetBlue");
        Grid.SetColumn(fill, 0);
        trackGrid.Children.Add(fill);
        track.Child = trackGrid;
        root.Children.Add(track);
        _flyoutProgressFillCol = fillCol;
        _flyoutProgressEmptyCol = emptyCol;

        // Compact 15 / 25 / 50 preset selector.
        var presetsGrid = new UniformGrid { Columns = FocusPresets.Length, Margin = new Thickness(0, 0, 0, 2) };
        var currentMins = (int)Math.Round(PhaseLength.TotalMinutes);
        for (var i = 0; i < FocusPresets.Length; i++)
        {
            var mins = FocusPresets[i];
            var isSelected = _state.CurrentPhase == Phase.Focus && currentMins == mins;
            var presetBtn = new Button
            {
                Content = UiText.Format("MinutesShort", mins),
                Margin = new Thickness(i == 0 ? 0 : 3, 0, i == FocusPresets.Length - 1 ? 0 : 3, 0),
                Padding = new Thickness(6, 5, 6, 5)
            };
            presetBtn.SetResourceReference(StyleProperty, isSelected ? "WidgetFlyoutPrimaryButton" : "WidgetFlyoutActionButton");
            presetBtn.Click += (_, _) =>
            {
                _state.CurrentPhase = Phase.Focus;
                _state.PresetMinutes = mins;
                _state.Remaining = TimeSpan.FromMinutes(mins);
                _state.FullLength = _state.Remaining;
                _state.StartedAt = null;
                _state.NotifyChanged();
                if (_flyoutContainer is not null) PopulateFocusFlyout(_flyoutContainer);
            };
            presetsGrid.Children.Add(presetBtn);
        }
        root.Children.Add(presetsGrid);

        // Unified footer bar with Reset + Start/Pause.
        var resetBtn = new Button { Content = UiText.Get("FocusReset") };
        resetBtn.SetResourceReference(StyleProperty, "WidgetFlyoutActionButton");
        resetBtn.Click += Reset_Click;

        var toggleBtn = new Button { Content = UiText.Get(IsRunning ? "FocusPause" : "FocusStart") };
        toggleBtn.SetResourceReference(StyleProperty, "WidgetFlyoutPrimaryButton");
        toggleBtn.Click += Toggle_Click;
        _flyoutToggleButton = toggleBtn;

        root.Children.Add(DockFlyout.FooterBar(resetBtn, toggleBtn));

        container.Child = root;
    }

    private void Toggle_Click(object sender, RoutedEventArgs e)
    {
        if (IsRunning)
        {
            _state.Remaining = TimeSpan.FromTicks(Math.Max(0, CurrentRemaining.Ticks));
            _state.StartedAt = null;
        }
        else
        {
            if (_state.Remaining <= TimeSpan.Zero) _state.Remaining = PhaseLength;
            _state.StartedAt = _clock.GetTimestamp();
        }

        _state.NotifyChanged();
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        _state.Number = 1;
        StartPhase(Phase.Focus, run: false);
        _state.NotifyChanged();
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
            _flyoutSessionCount.Text = UiText.Format("FocusSessionsCount", _state.Completed);
        }

        var lang = UiText.Current.Language;
        var phaseChanged = _renderedPhase != _state.CurrentPhase;
        var runningChanged = _renderedRunning != IsRunning;
        var langChanged = _renderedLanguage != lang;

        if (runningChanged)
        {
            ToggleGlyph.Content = FindResource(IsRunning ? "IconPauseFill" : "IconPlayFill");
            FocusIcon.SetResourceReference(ForegroundProperty, IsRunning ? "WidgetSelectedForeground" : "TextSecondary");
            if (IsRunning) PlayButton.SetResourceReference(BackgroundProperty, "WidgetSelectedFill");
            else PlayButton.ClearValue(BackgroundProperty);
            PlayButton.SetResourceReference(ForegroundProperty, IsRunning ? "WidgetSelectedForeground" : "TextPrimary");
        }

        if (runningChanged || langChanged)
        {
            var playLabel = UiText.Get(IsRunning ? "FocusPause" : "FocusStart");
            PlayButton.ToolTip = playLabel;
            AutomationProperties.SetName(PlayButton, playLabel);
            if (_flyoutToggleButton is not null) _flyoutToggleButton.Content = playLabel;
        }

        Card.ToolTip = $"{UiText.Get(_state.CurrentPhase == Phase.Focus ? "Focus" : "Break")} · {RemainingText.Text}";

        if ((phaseChanged || langChanged) && _flyoutContainer is not null && _flyout.IsOpen)
        {
            PopulateFocusFlyout(_flyoutContainer);
        }

        _renderedPhase = _state.CurrentPhase;
        _renderedRunning = IsRunning;
        _renderedLanguage = lang;
    }

    private void Pulse()
    {
        if (!Settings.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation) return;
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
