using System.ComponentModel;
using System.Media;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Data;
using System.Windows.Media.Animation;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Pomodoro-style focus timer: focus → break → next focus session, cycling through the configured session count.
/// A finished focus phase rolls straight into its break; a finished break waits for the user to start the next session.
/// </summary>
public partial class FocusWidget : DockWidget
{
    private enum Phase { Focus, Break }

    private Phase _phase = Phase.Focus;
    private int _session = 1;
    private TimeSpan _remaining;
    private TimeSpan _lastFullLength;
    private long? _startedAt;
    private readonly TimeProvider _clock;
    private bool _initialized;

    private int _renderedSeconds = -1;
    private Phase? _renderedPhase;
    private int _renderedSession = -1;
    private int _renderedSessionsTotal = -1;
    private bool? _renderedRunning;
    private AppLanguage? _renderedLanguage;

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 50;
    public override double FullWidthEstimate => 162;
    public override double CompactWidthEstimate => 100;

    public FocusWidget() : this(TimeProvider.System) { }

    public FocusWidget(TimeProvider clock)
    {
        _clock = clock;
        InitializeComponent();
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        PhaseRow.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        SkipButton.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        Card.Padding = compact ? new Thickness(10, 0, 6, 0) : new Thickness(12, 0, 8, 0);
    }

    public override UIElement? BuildHoverFlyoutContent()
    {
        var root = new StackPanel { Width = 200 };
        var heading = DockFlyout.Heading($"{PhaseText.Text} {SessionText.Text}".Trim());
        var headingBinding = new MultiBinding { StringFormat = "{0}{1}" };
        headingBinding.Bindings.Add(new Binding("Text") { Source = PhaseText });
        headingBinding.Bindings.Add(new Binding("Text") { Source = SessionText });
        heading.SetBinding(TextBlock.TextProperty, headingBinding);
        root.Children.Add(heading);
        var timer = new TextBlock { FontSize = 24, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 2, 0, 8) };
        timer.SetResourceReference(StyleProperty, "WidgetText");
        timer.SetBinding(TextBlock.TextProperty, new Binding("Text") { Source = RemainingText, Mode = BindingMode.OneWay });
        root.Children.Add(timer);

        var row = new StackPanel { Orientation = Orientation.Horizontal };
        var toggle = new Button { Width = 34, Height = 34, Content = new ContentControl { Content = FindResource(IsRunning ? "IconPauseFill" : "IconPlayFill"), Width = 14, Height = 14 } };
        toggle.SetResourceReference(StyleProperty, "WidgetIconButton");
        if (toggle.Content is ContentControl tc)
        {
            tc.SetResourceReference(StyleProperty, "WidgetGlyph");
            tc.SetBinding(ContentControl.ContentProperty, new Binding("Content") { Source = ToggleGlyph, Mode = BindingMode.OneWay });
        }
        toggle.SetBinding(Button.ToolTipProperty, new Binding("ToolTip") { Source = PlayButton, Mode = BindingMode.OneWay });
        System.Windows.Automation.AutomationProperties.SetName(toggle, UiText.Get(IsRunning ? "FocusPause" : "FocusStart"));
        toggle.Click += Toggle_Click;

        var skip = new Button { Width = 34, Height = 34, Margin = new Thickness(6, 0, 0, 0), Content = new ContentControl { Content = FindResource("IconSkip"), Width = 14, Height = 14 } };
        skip.SetResourceReference(StyleProperty, "WidgetIconButton");
        if (skip.Content is ContentControl sc) sc.SetResourceReference(StyleProperty, "WidgetIcon");
        skip.ToolTip = UiText.Get("FocusSkip");
        System.Windows.Automation.AutomationProperties.SetName(skip, UiText.Get("FocusSkip"));
        skip.Click += Skip_Click;

        var reset = new Button { Width = 34, Height = 34, Margin = new Thickness(6, 0, 0, 0), Content = new ContentControl { Content = FindResource("IconReset"), Width = 14, Height = 14 } };
        reset.SetResourceReference(StyleProperty, "WidgetIconButton");
        if (reset.Content is ContentControl rc) rc.SetResourceReference(StyleProperty, "WidgetIcon");
        reset.ToolTip = UiText.Get("FocusReset");
        System.Windows.Automation.AutomationProperties.SetName(reset, UiText.Get("FocusReset"));
        reset.Click += Reset_Click;

        row.Children.Add(toggle);
        row.Children.Add(skip);
        row.Children.Add(reset);
        root.Children.Add(row);
        return root;
    }

    private TimeSpan PhaseLength => TimeSpan.FromMinutes(_phase == Phase.Focus ? Settings.FocusMinutes : Settings.FocusBreakMinutes);
    private bool IsRunning => _startedAt is not null;
    private TimeSpan CurrentRemaining => _startedAt is { } startedAt
        ? _remaining - _clock.GetElapsedTime(startedAt)
        : _remaining;

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        Render();
    }

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        if (IsRunning)
        {
            _remaining = TimeSpan.FromTicks(Math.Max(0, CurrentRemaining.Ticks));
            _startedAt = null;
        }
        Card.BeginAnimation(OpacityProperty, null);
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (IsActive) Render();
    }

    protected override void OnSettingsChanged()
    {
        _session = Math.Min(_session, Settings.FocusSessions);
        // Idle timers follow edited lengths immediately; a running phase keeps its deadline.
        if (!_initialized || (!IsRunning && _remaining == _lastFullLength))
        {
            _remaining = PhaseLength;
            _initialized = true;
        }

        _lastFullLength = PhaseLength;
        Render();
    }

    /// <summary>Renders the configured focus duration locally; no timers or sensors are started.</summary>
    public override bool SupportsStaticPreview => true;
    protected override void OnRenderPreview() => Render();

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

    private void Skip_Click(object sender, RoutedEventArgs e)
    {
        var wasRunning = IsRunning;
        if (_phase == Phase.Focus)
        {
            StartPhase(Phase.Break, wasRunning);
        }
        else
        {
            _session = _session >= Settings.FocusSessions ? 1 : _session + 1;
            StartPhase(Phase.Focus, wasRunning);
        }

        Render();
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        _session = 1;
        StartPhase(Phase.Focus, run: false);
        Render();
    }

    private void Render()
    {
        var elapsed = CurrentRemaining;
        var remaining = elapsed < TimeSpan.Zero ? TimeSpan.Zero : elapsed;
        // Round up so the display reads 25:00 at the start and 00:00 only at the end.
        var seconds = (int)Math.Ceiling(remaining.TotalSeconds);
        if (_renderedSeconds != seconds)
        {
            _renderedSeconds = seconds;
            RemainingText.Text = $"{seconds / 60:00}:{seconds % 60:00}";
        }

        var lang = UiText.Current.Language;
        var phaseChanged = _renderedPhase != _phase;
        var runningChanged = _renderedRunning != IsRunning;
        var langChanged = _renderedLanguage != lang;

        if (phaseChanged || langChanged)
        {
            PhaseText.Text = UiText.Get(_phase == Phase.Focus ? "Focus" : "Break");
        }

        if (_renderedSession != _session || _renderedSessionsTotal != Settings.FocusSessions)
        {
            _renderedSession = _session;
            _renderedSessionsTotal = Settings.FocusSessions;
            SessionText.Text = $" {_session}/{Settings.FocusSessions}";
        }

        if (runningChanged)
        {
            ToggleGlyph.Content = FindResource(IsRunning ? "IconPauseFill" : "IconPlayFill");
        }

        if (runningChanged || langChanged)
        {
            var playLabel = UiText.Get(IsRunning ? "FocusPause" : "FocusStart");
            PlayButton.ToolTip = playLabel;
            AutomationProperties.SetName(PlayButton, playLabel);
        }

        if (runningChanged || phaseChanged)
        {
            Card.SetResourceReference(Border.BorderBrushProperty,
                IsRunning ? (_phase == Phase.Focus ? "WidgetFocusAccent" : "WidgetAccent") : "WidgetTrack");
        }

        if (phaseChanged)
        {
            PhaseText.SetResourceReference(TextElement.ForegroundProperty,
                _phase == Phase.Focus ? "WidgetFocusAccent" : "WidgetAccent");
        }

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
}
