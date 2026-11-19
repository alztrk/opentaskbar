using System.Diagnostics;
using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>One CLI quota meter for a single provider; it only polls while the widget is enabled.</summary>
public sealed class AiUsageWidget : DockWidget
{
    private readonly AiProviders _provider;
    private readonly Func<AiProviders, CancellationToken, Task<(AiUsageReport?, string?)>>? _loader;
    private AiUsageReport? _report;
    private string? _error;
    private DateTimeOffset? _nextLoadAt;
    private DateTime? _signInStamp;
    private int _failures;
    private DateTimeOffset? _lastLoadAt;
    // The click flyout and the compact hover flyout can coexist, so each keeps its own countdowns.
    private readonly List<(TextBlock Block, DateTimeOffset Reset)> _countdowns = [];
    private readonly List<(TextBlock Block, DateTimeOffset Reset)> _hoverCountdowns = [];
    private readonly ContentControl _flyoutHost = new();
    private readonly Card _card;
    private readonly DockFlyout _flyout = new();
    private CancellationTokenSource? _loading;
    private int _renderedMinute = -1;

    private sealed record DualQuotaBars(
        TextBlock SessionLabel,
        Border SessionTrack,
        Border SessionFill,
        TextBlock SessionReset,
        TextBlock WeeklyLabel,
        Border WeeklyTrack,
        Border WeeklyFill,
        TextBlock WeeklyReset);

    private sealed class AntigravityQuotaSlot(
        Border Fill,
        TextBlock Reset,
        DateTimeOffset? ResetsAt,
        bool IsWeekly)
    {
        public Border Fill { get; } = Fill;
        public TextBlock Reset { get; } = Reset;
        public DateTimeOffset? ResetsAt { get; } = ResetsAt;
        public bool IsWeekly { get; } = IsWeekly;
    }

    private sealed class AntigravityColumn(
        FrameworkElement Element,
        IReadOnlyList<AntigravityQuotaSlot> Slots)
    {
        public FrameworkElement Element { get; } = Element;
        public IReadOnlyList<AntigravityQuotaSlot> Slots { get; } = Slots;
    }

    private sealed record AntigravityCard(
        StackPanel Container,
        List<AntigravityColumn> Columns);

    private sealed record Card(
        Button Button,
        System.Windows.Shapes.Path Logo,
        TextBlock Name,
        RingMeter? Ring,
        DualQuotaBars? Bars,
        AntigravityCard? Antigravity)
    {
        public DualQuotaBars? Codex => Bars;
    }

    private static bool UsesDualQuota(AiProviders provider) => provider is AiProviders.Codex or AiProviders.Claude;
    private static bool HidesName(AiProviders provider) => provider is AiProviders.Codex or AiProviders.Claude or AiProviders.Antigravity;
    private static bool UsesCustomCard(AiProviders provider) => provider is AiProviders.Codex or AiProviders.Claude or AiProviders.Antigravity;

    public AiUsageWidget(AiProviders provider, Func<AiProviders, CancellationToken, Task<(AiUsageReport?, string?)>>? loader = null)
    {
        if (!Enum.IsDefined(provider) || provider is AiProviders.None or AiProviders.All)
            throw new ArgumentOutOfRangeException(nameof(provider), provider, "A single AI provider is required.");
        _provider = provider;
        _loader = loader;
        _card = AddCard();
        _flyout.Closed += (_, _) => _countdowns.Clear();
        Content = _card.Button;
        Render();
    }

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 40;
    // Card layout: padding and border, logo, name, ring; the compact card drops the name.
    private const double LogoSize = 16, NameWidth = 44, RingSize = 34, Gap = 6;
    private const double CardPaddingAndBorder = 22;
    public override double FullWidthEstimate => CardWidth(_provider, false) + CardSpacing;
    public override double CompactWidthEstimate => CardWidth(_provider, true) + CardSpacing;
    private static double CardWidth(AiProviders provider, bool compact) =>
        provider switch
        {
            AiProviders.Codex or AiProviders.Claude =>
                CardPaddingAndBorder + LogoSize + Gap + 120,
            AiProviders.Antigravity =>
                CardPaddingAndBorder + LogoSize + Gap + 220,
            _ =>
                CardPaddingAndBorder + LogoSize + Gap + (compact ? 0 : NameWidth + Gap) + RingSize
        };
    private double CardSpacing => TryFindResource("WidgetSpacing") is Thickness spacing ? spacing.Left + spacing.Right : 0;

    protected override void OnActivated()
    {
        Render();
        _ = LoadAsync();
    }

    protected override void OnDeactivated()
    {
        Clear();
        _flyout.Close();
    }

    private void Clear()
    {
        _loading?.Cancel();
        _loading = null;
        _report = null;
        _error = null;
        _nextLoadAt = null;
        _signInStamp = null;
        _failures = 0;
        _lastLoadAt = null;
    }

    protected override void OnSettingsChanged() => Render();
    protected override void OnLanguageChanged() => Refresh();
    protected override void OnCompactChanged(bool compact) => Render();
    public override UIElement? BuildHoverFlyoutContent() => UsesCustomCard(_provider) ? null : BuildFlyout(_hoverCountdowns);

    protected override void OnTick(DateTime now)
    {
        if (_loading is null) _ = LoadAsync();
        // Reset times and staleness only change by the minute; open flyouts keep counting down.
        if (now.Minute != _renderedMinute) Render();
        if (_card.Bars is not null) UpdateDualCountdowns();
        if (_card.Antigravity is not null) UpdateAntigravityCountdowns();
        if (!IsHoverFlyoutOpen) _hoverCountdowns.Clear();
        foreach (var (block, reset) in _countdowns.Concat(_hoverCountdowns)) block.Text = ResetText(reset);
    }

    private void UpdateDualCountdowns()
    {
        if (_card.Bars is not { } bars || _report is not { } report) return;
        if (report.Windows.FirstOrDefault(w => w.LabelKey == "AiUsageSession")?.ResetsAt is { } sReset)
        {
            var sRem = sReset - DateTimeOffset.UtcNow;
            bars.SessionReset.Text = sRem > TimeSpan.Zero ? Duration(sRem) : Duration(TimeSpan.Zero);
        }
        if (report.Windows.FirstOrDefault(w => w.LabelKey == "AiUsageWeekly")?.ResetsAt is { } wReset)
        {
            bars.WeeklyReset.Text = When(wReset);
        }
    }

    private void UpdateAntigravityCountdowns()
    {
        if (_card.Antigravity is not { } agy) return;
        foreach (var col in agy.Columns)
        {
            foreach (var slot in col.Slots)
            {
                if (slot.ResetsAt is { } reset)
                {
                    var rem = reset - DateTimeOffset.UtcNow;
                    slot.Reset.Text = !slot.IsWeekly && rem < RelativeLimit
                        ? (rem > TimeSpan.Zero ? Duration(rem) : Duration(TimeSpan.Zero))
                        : When(reset);
                }
            }
        }
    }

    /// <summary>Re-renders the card and, when our click flyout is open, swaps its content in place without reopening it.</summary>
    private void Refresh()
    {
        Render();
        if (_flyout.IsOpen) _flyoutHost.Content = BuildFlyout(_countdowns);
    }

    private static bool IsStale(AiUsageReport report) => DateTimeOffset.UtcNow - report.FetchedAt > TimeSpan.FromMinutes(10);

    private bool IsDue(DateTime credentialStamp)
    {
        if (_signInStamp is { } stamp)
        {
            if (_loader is not null || credentialStamp == stamp) return false;
            _signInStamp = null;
            _nextLoadAt = null;
        }
        return _nextLoadAt is not { } next || DateTimeOffset.UtcNow >= next;
    }

    private async Task LoadAsync()
    {
        if (!IsActive || _loading is not null) return;
        if (_signInStamp is null && _nextLoadAt is { } scheduled && DateTimeOffset.UtcNow < scheduled) return;
        var loading = new CancellationTokenSource();
        _loading = loading;
        bool Superseded() => !ReferenceEquals(_loading, loading) || !IsActive || loading.IsCancellationRequested;
        try
        {
            var stamp = _loader is null ? await Task.Run(() => AiUsageService.CredentialStamp(_provider), loading.Token) : DateTime.MinValue;
            if (Superseded() || !IsDue(stamp)) return;
            _lastLoadAt = DateTimeOffset.UtcNow;
            AiUsageService.LoadResult result;
            if (_loader is null) result = await AiUsageService.LoadWithPolicyAsync(_provider, loading.Token);
            else
            {
                var (report, error) = await _loader(_provider, loading.Token);
                result = new(report, error);
            }
            if (Superseded()) return;
            if (result.Report is { } loaded) _report = loaded;
            _error = result.Error;
            if (result.Error == "ErrorAiUsageSignIn") _signInStamp = stamp;
            var failures = result.Error is null ? 0 : _failures + 1;
            _failures = Math.Min(failures, 10);
            _nextLoadAt = DateTimeOffset.UtcNow + RetryDelay(result.Error, failures, result.RetryAfter);
            Refresh();
        }
        catch (OperationCanceledException) when (loading.IsCancellationRequested) { }
        catch (Exception ex)
        {
            if (Superseded()) return;
            _error = "ErrorAiUsageLoad";
            _failures = Math.Min(10, _failures + 1);
            _nextLoadAt = DateTimeOffset.UtcNow + RetryDelay("ErrorAiUsageLoad", _failures, null);
            AppLog.Info($"AI usage unavailable: {ex.GetType().Name}");
        }
        finally
        {
            if (ReferenceEquals(_loading, loading)) _loading = null;
            loading.Dispose();
        }
    }

    internal static TimeSpan RetryDelay(string? error, int failures, TimeSpan? retryAfter)
    {
        if (retryAfter is { } specified) return specified > TimeSpan.Zero ? specified : TimeSpan.FromSeconds(1);
        if (error is null) return TimeSpan.FromSeconds(300 + Random.Shared.Next(31));
        var minutes = error == "ErrorAiUsageRateLimited" ? 5 : 2;
        return TimeSpan.FromMinutes(Math.Min(30, minutes * Math.Pow(2, Math.Clamp(failures - 1, 0, 10))));
    }

    private static string ProviderName(AiProviders provider) => provider switch
    {
        AiProviders.Claude => "Claude Code",
        AiProviders.Codex => "Codex",
        AiProviders.OpenCode => "OpenCode Go",
        AiProviders.Copilot => "GitHub Copilot",
        AiProviders.DeepSeek => "DeepSeek",
        AiProviders.OpenRouter => "OpenRouter",
        _ => "Antigravity"
    };
    private string ErrorText(AiProviders provider, string key) => key == "ErrorAiUsageSignIn"
        ? UiText.Format(key, provider switch
        {
            AiProviders.Claude => "claude auth login",
            AiProviders.Codex => "codex login",
            AiProviders.OpenCode => "opencode auth login",
            AiProviders.Copilot => "gh auth login",
            AiProviders.DeepSeek => "DEEPSEEK_API_KEY",
            AiProviders.OpenRouter => "OPENROUTER_API_KEY",
            _ => "agy"
        }) : UiText.Get(key);
    private CultureInfo Culture => Settings.Language.Culture;
    private static bool IsModel(AiUsageWindow window) => window.LabelKey.StartsWith("AiUsageWeeklyModel:", StringComparison.Ordinal);
    private static bool IsPool(AiUsageWindow window) => window.LabelKey.StartsWith("AiUsagePool:", StringComparison.Ordinal);
    private static bool IsLimitReached(AiUsageReport report) => report.LimitReached
        || (report.Provider != AiProviders.Antigravity && report.Windows.Any(window => window.Used >= 1 && !IsModel(window)));
    private static string Percent(double value) => UiText.Format("Percent",
        ((int)Math.Round(value * 100)).ToString(CultureInfo.InvariantCulture));

    /// <summary>Accent for a usage level; a provider severity can raise it but never hide a high reading.</summary>
    internal static string Accent(double value, string? severity = null)
    {
        var level = value >= 0.9 ? 2 : value >= 0.75 ? 1 : 0;
        if (severity is not null && !severity.Equals("normal", StringComparison.OrdinalIgnoreCase))
            level = Math.Max(level, severity.Contains("warn", StringComparison.OrdinalIgnoreCase) ? 1
                : severity.Contains("crit", StringComparison.OrdinalIgnoreCase)
                    || severity.Contains("exceed", StringComparison.OrdinalIgnoreCase)
                    || severity.Contains("block", StringComparison.OrdinalIgnoreCase) ? 2 : 0);
        return level == 2 ? "WidgetFocusAccent" : level == 1 ? "WidgetWarning" : "WidgetAccent";
    }

    /// <summary>The window the dock ring shows; a missing session or weekly window falls back to the highest.</summary>
    internal static AiUsageWindow? RingWindow(AiUsageReport report, AiUsageRing ring) => ring switch
    {
        AiUsageRing.Session => report.Windows.Where(window => window.LabelKey == "AiUsageSession").MaxBy(window => window.Used),
        AiUsageRing.Weekly => report.Windows.Where(window => window.LabelKey == "AiUsageWeekly").MaxBy(window => window.Used),
        _ => null
    } ?? Highest(report);

    /// <summary>The fullest session or weekly window; per-model windows only count when nothing else is reported.</summary>
    private static AiUsageWindow? Highest(AiUsageReport report) =>
        report.Windows.Where(window => !IsModel(window)).MaxBy(window => window.Used) ?? report.Windows.MaxBy(window => window.Used);

    /// <summary>Status for a loaded report: limit reached, then the provider severity or the 75%/90% thresholds.</summary>
    internal static (string Key, string Brush) Status(AiUsageReport report)
    {
        if (IsLimitReached(report)) return ("AiUsageLimitReached", "WidgetFocusAccent");
        var level = report.Windows.Where(window => !IsModel(window)).Select(window => Accent(window.Used, window.Severity)).ToArray();
        return level.Contains("WidgetFocusAccent") ? ("AiUsageStatusNearLimit", "WidgetFocusAccent")
            : level.Contains("WidgetWarning") ? ("AiUsageStatusHigh", "WidgetWarning") : ("AiUsageStatusOk", "WidgetAccent");
    }

    /// <summary>Only the window that would run out soonest carries a pace warning, so one provider shows at most one.</summary>
    internal static AiUsageWindow? PaceWindow(AiUsageReport report, DateTimeOffset now) => report.Windows
        .Select(window => (Window: window, Limit: LimitIn(window, now))).Where(item => item.Limit is not null)
        .OrderBy(item => item.Limit).Select(item => item.Window).FirstOrDefault();

    /// <summary>Share of the window that has elapsed, or null when its length or reset time is unknown.</summary>
    internal static double? Elapsed(AiUsageWindow window, DateTimeOffset now)
    {
        if (window.WindowSeconds is not { } length || length <= 0 || window.ResetsAt is not { } reset) return null;
        var remaining = (reset - now).TotalSeconds;
        return remaining <= 0 || remaining > length ? null : 1 - remaining / length;
    }

    /// <summary>Time until the limit at the window's average rate so far, only when that comes before the reset.</summary>
    internal static TimeSpan? LimitIn(AiUsageWindow window, DateTimeOffset now)
    {
        // The first minutes of a window say little about the rate (5% is 15 minutes of a five-hour session).
        if (Elapsed(window, now) is not { } elapsed || elapsed < 0.05 || window.Used is <= 0 or >= 1
            || window.WindowSeconds is not { } seconds) return null;
        var length = (double)seconds;
        var toLimit = (1 - window.Used) * elapsed * length / window.Used;
        return toLimit < (1 - elapsed) * length ? TimeSpan.FromSeconds(Math.Max(60, toLimit)) : null;
    }

    private string Label(AiUsageWindow window)
    {
        if (IsPool(window)) return window.LabelKey[12..];
        if (!IsModel(window)) return UiText.Get(window.LabelKey);
        var model = window.LabelKey.Split(':', 2)[1].Replace('_', ' ');
        return UiText.Format("AiUsageWeeklyModel", model.Length == 0 ? model : char.ToUpper(model[0], Culture) + model[1..]);
    }

    private static string GroupLabel(string group) =>
        group.StartsWith("AiUsageGroup", StringComparison.Ordinal) ? UiText.Get(group) : group;

    private string SummaryWindowLabel(AiUsageWindow window) =>
        window.Group is { Length: > 0 } group ? $"{GroupLabel(group)} ({Label(window)})" : Label(window);

    // "h:mm" reads as a clock time once it nears a day, so longer spans show the local weekday and time instead.
    private static readonly TimeSpan RelativeLimit = TimeSpan.FromHours(6);

    private string ResetText(DateTimeOffset reset)
    {
        var remaining = reset - DateTimeOffset.UtcNow;
        return remaining < RelativeLimit
            ? UiText.Format("AiUsageResetsIn", Duration(remaining))
            : UiText.Format("AiUsageResetsAt", When(reset));
    }

    private string When(DateTimeOffset moment)
    {
        var local = moment.ToLocalTime();
        return moment - DateTimeOffset.UtcNow < TimeSpan.FromDays(6)
            ? $"{local.ToString("ddd", Culture)} {local.ToString("t", Culture)}" : local.ToString("g", Culture);
    }

    private string Duration(TimeSpan span)
    {
        var value = span < TimeSpan.Zero ? TimeSpan.Zero : span;
        var minutes = Math.Max(1, (int)Math.Ceiling(value.TotalMinutes));
        // Spelled-out units: a bare "1:54" reads as a clock time, not a duration.
        return minutes < 60 ? UiText.Format("MinutesFormat", minutes) : UiText.Format("HoursMinutesFormat", minutes / 60, minutes % 60);
    }

    private string Summary()
    {
        var name = ProviderName(_provider);
        if (_report is not { } report || RingWindow(report, Settings.AiUsageRing) is not { } main)
            return _error is null ? name : $"{name}: {ErrorText(_provider, _error)}";
        var parts = new List<string> { $"{name} {Percent(main.Used)}" };
        parts.AddRange(report.Windows.Where(window => window != main && !IsModel(window))
            .Select(window => $"{SummaryWindowLabel(window)} {Percent(window.Used)}"));
        if (IsLimitReached(report)) parts.Add(UiText.Get("AiUsageLimitReached"));
        else if (main.ResetsAt is { } reset) parts.Add(ResetText(reset));
        if (_error is not null) parts.Add(ErrorText(_provider, _error));
        return string.Join(" · ", parts);
    }

    private static string ShortName(AiProviders provider) => provider switch
    {
        AiProviders.Claude => "Claude",
        AiProviders.OpenCode => "OpenCode",
        AiProviders.Copilot => "Copilot",
        _ => ProviderName(provider)
    };

    private static string LogoKey(AiProviders provider) => provider switch
    {
        AiProviders.Claude => "LogoClaude",
        AiProviders.Codex => "LogoCodex",
        AiProviders.OpenCode => "LogoOpenCode",
        AiProviders.Copilot => "LogoCopilot",
        AiProviders.DeepSeek => "LogoDeepSeek",
        AiProviders.OpenRouter => "LogoOpenRouter",
        _ => "LogoAntigravity"
    };

    private Card AddCard()
    {
        var logo = new System.Windows.Shapes.Path { Width = LogoSize, Height = LogoSize, Stretch = Stretch.Uniform,
            Margin = new Thickness(0, 0, Gap, 0), VerticalAlignment = VerticalAlignment.Center, IsHitTestVisible = false,
            Data = (Geometry)TryFindResource(LogoKey(_provider)) };
        logo.SetResourceReference(System.Windows.Shapes.Shape.FillProperty, "TextPrimary");
        var label = Text(ShortName(_provider), 12, "TextPrimary");
        label.FontWeight = FontWeights.SemiBold;
        label.Margin = new Thickness(0, 0, Gap, 0);

        if (_provider == AiProviders.Antigravity)
        {
            label.Visibility = Visibility.Collapsed;
            var container = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
            var agyCard = new AntigravityCard(container, new List<AntigravityColumn>());
            RebuildAntigravity(agyCard, false);

            var row = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center, Children = { logo, label, container } };
            var button = new Button { Content = row, Cursor = System.Windows.Input.Cursors.Arrow };
            button.SetResourceReference(StyleProperty, "WidgetCardButton");
            return new(button, logo, label, null, null, agyCard);
        }

        if (UsesDualQuota(_provider))
        {
            label.Visibility = Visibility.Collapsed;
            var sessionLabel = Text(UiText.Get("AiUsageSessionShort"), 9.5, "TextSecondary");
            sessionLabel.FontWeight = FontWeights.SemiBold;
            sessionLabel.Width = 18;
            sessionLabel.VerticalAlignment = VerticalAlignment.Center;
            Typography.SetNumeralAlignment(sessionLabel, FontNumeralAlignment.Tabular);

            var sessionTrack = new Border { Width = 38, Height = 4, CornerRadius = new CornerRadius(2), VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left };
            sessionTrack.SetResourceReference(Border.BackgroundProperty, "TextMuted");
            sessionTrack.Opacity = 0.35;

            var sessionFill = new Border { Height = 4, CornerRadius = new CornerRadius(2), VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left, Width = 0 };
            sessionFill.SetResourceReference(Border.BackgroundProperty, "WidgetAccent");

            var sessionBar = new Grid { Width = 38, Height = 6, Margin = new Thickness(3, 0, 5, 0), VerticalAlignment = VerticalAlignment.Center };
            sessionBar.Children.Add(sessionTrack);
            sessionBar.Children.Add(sessionFill);

            var sessionReset = Text("—", 9, "TextSecondary");
            sessionReset.VerticalAlignment = VerticalAlignment.Center;
            Typography.SetNumeralAlignment(sessionReset, FontNumeralAlignment.Tabular);

            var sessionRow = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 0, 2) };
            sessionRow.Children.Add(sessionLabel);
            sessionRow.Children.Add(sessionBar);
            sessionRow.Children.Add(sessionReset);

            var weeklyLabel = Text(UiText.Get("AiUsageWeeklyShort"), 9.5, "TextSecondary");
            weeklyLabel.FontWeight = FontWeights.SemiBold;
            weeklyLabel.Width = 18;
            weeklyLabel.VerticalAlignment = VerticalAlignment.Center;
            Typography.SetNumeralAlignment(weeklyLabel, FontNumeralAlignment.Tabular);

            var weeklyTrack = new Border { Width = 38, Height = 4, CornerRadius = new CornerRadius(2), VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left };
            weeklyTrack.SetResourceReference(Border.BackgroundProperty, "TextMuted");
            weeklyTrack.Opacity = 0.35;

            var weeklyFill = new Border { Height = 4, CornerRadius = new CornerRadius(2), VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Left, Width = 0 };
            weeklyFill.SetResourceReference(Border.BackgroundProperty, "WidgetAccent");

            var weeklyBar = new Grid { Width = 38, Height = 6, Margin = new Thickness(3, 0, 5, 0), VerticalAlignment = VerticalAlignment.Center };
            weeklyBar.Children.Add(weeklyTrack);
            weeklyBar.Children.Add(weeklyFill);

            var weeklyReset = Text("—", 9, "TextSecondary");
            weeklyReset.VerticalAlignment = VerticalAlignment.Center;
            Typography.SetNumeralAlignment(weeklyReset, FontNumeralAlignment.Tabular);

            var weeklyRow = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
            weeklyRow.Children.Add(weeklyLabel);
            weeklyRow.Children.Add(weeklyBar);
            weeklyRow.Children.Add(weeklyReset);

            var barsPanel = new StackPanel { Orientation = Orientation.Vertical, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(2, 0, 0, 0) };
            barsPanel.Children.Add(sessionRow);
            barsPanel.Children.Add(weeklyRow);

            var dualBars = new DualQuotaBars(sessionLabel, sessionTrack, sessionFill, sessionReset, weeklyLabel, weeklyTrack, weeklyFill, weeklyReset);

            var row = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center, Children = { logo, label, barsPanel } };
            var button = new Button { Content = row, Cursor = System.Windows.Input.Cursors.Arrow };
            button.SetResourceReference(StyleProperty, "WidgetCardButton");
            // Click popup menu is removed for dual quota widgets (Codex, Claude)
            return new(button, logo, label, null, dualBars, null);
        }

        var ring = new RingMeter { Width = RingSize, Height = RingSize, StrokeThickness = 3, FontSize = 10.5, Text = "—", IsHitTestVisible = false };
        ring.SetResourceReference(RingMeter.TrackBrushProperty, "TextMuted");
        var defaultRow = new StackPanel { Orientation = Orientation.Horizontal, Children = { logo, label, ring } };
        var defaultButton = new Button { Content = defaultRow };
        defaultButton.SetResourceReference(StyleProperty, "WidgetCardButton");
        defaultButton.Click += (_, _) => _flyout.Toggle(defaultButton, () =>
        {
            _flyoutHost.Content = BuildFlyout(_countdowns);
            return _flyoutHost;
        });
        return new(defaultButton, logo, label, ring, null, null);
    }

    private void Render()
    {
        HasData = true;
        _renderedMinute = DateTime.Now.Minute;
        if (!HidesName(_provider))
            _card.Name.Visibility = IsCompact ? Visibility.Collapsed : Visibility.Visible;
        var main = _report is { } report ? RingWindow(report, Settings.AiUsageRing) : null;
        UpdateCard(main, _report is { } loaded && IsStale(loaded), Summary());
    }

    private void UpdateCard(AiUsageWindow? main, bool stale, string toolTip)
    {
        var foreground = stale ? "TextMuted" : "TextPrimary";
        var secondary = stale ? "TextMuted" : "TextSecondary";
        _card.Name.SetResourceReference(TextBlock.ForegroundProperty, foreground);
        _card.Logo.SetResourceReference(System.Windows.Shapes.Shape.FillProperty, foreground);
        _card.Button.ToolTip = toolTip;
        AutomationProperties.SetName(_card.Button, $"{UiText.Get("WidgetAiUsage")}: {toolTip}");

        if (_card.Antigravity is { } agy)
        {
            RebuildAntigravity(agy, stale);
        }
        else if (_card.Bars is { } bars)
        {
            bars.SessionLabel.Text = UiText.Get("AiUsageSessionShort");
            bars.WeeklyLabel.Text = UiText.Get("AiUsageWeeklyShort");
            bars.SessionLabel.SetResourceReference(TextBlock.ForegroundProperty, secondary);
            bars.WeeklyLabel.SetResourceReference(TextBlock.ForegroundProperty, secondary);

            var session = _report?.Windows.FirstOrDefault(w => w.LabelKey == "AiUsageSession");
            var weekly = _report?.Windows.FirstOrDefault(w => w.LabelKey == "AiUsageWeekly");

            var sUsed = session?.Used ?? 0;
            bars.SessionFill.Width = Math.Max(0, Math.Min(38, 38 * sUsed));
            bars.SessionFill.SetResourceReference(Border.BackgroundProperty, session is null ? "TextMuted" : Accent(sUsed, session.Severity));

            if (session?.ResetsAt is { } sReset)
            {
                var sRem = sReset - DateTimeOffset.UtcNow;
                bars.SessionReset.Text = sRem > TimeSpan.Zero ? Duration(sRem) : Duration(TimeSpan.Zero);
            }
            else
            {
                bars.SessionReset.Text = "—";
            }
            bars.SessionReset.SetResourceReference(TextBlock.ForegroundProperty, secondary);

            var wUsed = weekly?.Used ?? 0;
            bars.WeeklyFill.Width = Math.Max(0, Math.Min(38, 38 * wUsed));
            bars.WeeklyFill.SetResourceReference(Border.BackgroundProperty, weekly is null ? "TextMuted" : Accent(wUsed, weekly.Severity));

            if (weekly?.ResetsAt is { } wReset)
            {
                bars.WeeklyReset.Text = When(wReset);
            }
            else
            {
                bars.WeeklyReset.Text = "—";
            }
            bars.WeeklyReset.SetResourceReference(TextBlock.ForegroundProperty, secondary);
        }
        else if (_card.Ring is { } ring)
        {
            ring.Value = main?.Used ?? 0;
            ring.Text = main is null ? "—" : ((int)Math.Round(main.Used * 100)).ToString(CultureInfo.InvariantCulture);
            ring.SetResourceReference(RingMeter.ValueBrushProperty, main is null ? "TextMuted" : Accent(main.Used, main.Severity));
            ring.SetResourceReference(RingMeter.ForegroundProperty, foreground);
        }
    }

    private static Border CreateVerticalDivider()
    {
        var divider = new Border
        {
            Width = 1,
            Height = 24,
            Margin = new Thickness(5, 0, 5, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Opacity = 0.35
        };
        divider.SetResourceReference(Border.BackgroundProperty, "TextMuted");
        return divider;
    }

    private static string AntigravityGroupHeader(string group) => group switch
    {
        "AiUsageGroupGemini" or "Gemini Models" => "Gemini",
        "AiUsageGroupClaudeGpt" or "Claude and GPT models" => "Claude",
        _ when group.Contains("Argon", StringComparison.OrdinalIgnoreCase) => "Argon",
        _ when group.StartsWith("Gemini", StringComparison.OrdinalIgnoreCase) && group.Length > 6
            => group[6..].Trim(),
        _ => group.Split(' ')[0]
    };

    private static string AntigravityPoolHeader(string labelKey) => labelKey switch
    {
        "AiUsagePool:Gemini Pro" => "Pro",
        "AiUsagePool:Gemini Flash" => "Flash",
        "AiUsagePool:Claude" => "Claude",
        "AiUsagePool:GPT-OSS" => "GPT",
        _ when labelKey.StartsWith("AiUsagePool:", StringComparison.Ordinal)
            => labelKey[12..].Split(' ')[^1],
        _ => labelKey
    };

    private void RebuildAntigravity(AntigravityCard agy, bool stale)
    {
        agy.Container.Children.Clear();
        agy.Columns.Clear();
        var secondary = stale ? "TextMuted" : "TextSecondary";

        if (_report is not { } report || report.Windows.Count == 0)
        {
            BuildAntigravityPlaceholder(agy, secondary);
            return;
        }

        var hasGroups = report.Windows.Any(w => w.Group is not null);
        if (hasGroups)
        {
            var groups = report.Windows
                .Where(w => w.Group is not null)
                .GroupBy(w => w.Group!, StringComparer.Ordinal)
                .Where(g => g.Any())
                .ToList();

            for (var i = 0; i < groups.Count; i++)
            {
                if (i > 0) agy.Container.Children.Add(CreateVerticalDivider());
                var col = BuildAntigravityGroupColumn(groups[i].Key, groups[i].ToList(), secondary);
                agy.Columns.Add(col);
                agy.Container.Children.Add(col.Element);
            }
        }
        else
        {
            for (var i = 0; i < report.Windows.Count; i++)
            {
                if (i > 0) agy.Container.Children.Add(CreateVerticalDivider());
                var col = BuildAntigravityPoolColumn(report.Windows[i], secondary);
                agy.Columns.Add(col);
                agy.Container.Children.Add(col.Element);
            }
        }
    }

    private void BuildAntigravityPlaceholder(AntigravityCard agy, string secondary)
    {
        var col1 = BuildAntigravityPlaceholderColumn("Gemini", secondary, hasSession: true);
        agy.Columns.Add(col1);
        agy.Container.Children.Add(col1.Element);

        agy.Container.Children.Add(CreateVerticalDivider());

        var col2 = BuildAntigravityPlaceholderColumn("Claude", secondary, hasSession: false);
        agy.Columns.Add(col2);
        agy.Container.Children.Add(col2.Element);
    }

    private AntigravityColumn BuildAntigravityPlaceholderColumn(string label, string secondary, bool hasSession = true)
    {
        var slots = new List<AntigravityQuotaSlot>();
        var rows = new StackPanel { Orientation = Orientation.Vertical, VerticalAlignment = VerticalAlignment.Center };

        if (hasSession)
        {
            var (sRow, sSlot) = CreateAntigravityBarRow(UiText.Get("AiUsageSessionShort"), null, false, secondary, true);
            rows.Children.Add(sRow);
            slots.Add(sSlot);
        }

        var (wRow, wSlot) = CreateAntigravityBarRow(UiText.Get("AiUsageWeeklyShort"), null, true, secondary, false);
        rows.Children.Add(wRow);
        slots.Add(wSlot);

        var header = Text(label, 9.5, secondary);
        header.FontWeight = FontWeights.SemiBold;
        header.HorizontalAlignment = HorizontalAlignment.Left;
        header.Margin = new Thickness(0, 0, 0, 1.5);
        Typography.SetNumeralAlignment(header, FontNumeralAlignment.Tabular);

        var colPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(2, 0, 2, 0)
        };
        colPanel.Children.Add(header);
        colPanel.Children.Add(rows);
        return new(colPanel, slots);
    }

    private (StackPanel Row, AntigravityQuotaSlot Slot) CreateAntigravityBarRow(
        string label, AiUsageWindow? window, bool isWeekly, string secondary, bool hasBottomMargin)
    {
        var lbl = Text(label, 9, secondary);
        lbl.FontWeight = FontWeights.SemiBold;
        lbl.Width = 16;
        lbl.VerticalAlignment = VerticalAlignment.Center;
        Typography.SetNumeralAlignment(lbl, FontNumeralAlignment.Tabular);

        var track = new Border
        {
            Width = 32,
            Height = 4,
            CornerRadius = new CornerRadius(2),
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Left,
            Opacity = 0.35
        };
        track.SetResourceReference(Border.BackgroundProperty, "TextMuted");

        var used = window?.Used ?? 0;
        var fill = new Border
        {
            Height = 4,
            CornerRadius = new CornerRadius(2),
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Left,
            Width = Math.Max(0, Math.Min(32, 32 * used))
        };
        fill.SetResourceReference(Border.BackgroundProperty, window is null ? "TextMuted" : Accent(used, window.Severity));

        var bar = new Grid
        {
            Width = 32,
            Height = 6,
            Margin = new Thickness(2, 0, 4, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        bar.Children.Add(track);
        bar.Children.Add(fill);

        string resetStr;
        if (window?.ResetsAt is { } reset)
        {
            var rem = reset - DateTimeOffset.UtcNow;
            resetStr = !isWeekly && rem < RelativeLimit
                ? (rem > TimeSpan.Zero ? Duration(rem) : Duration(TimeSpan.Zero))
                : When(reset);
        }
        else
        {
            resetStr = "—";
        }

        var resetText = Text(resetStr, 8.5, secondary);
        resetText.VerticalAlignment = VerticalAlignment.Center;
        Typography.SetNumeralAlignment(resetText, FontNumeralAlignment.Tabular);

        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 0, hasBottomMargin ? 1.5 : 0)
        };
        row.Children.Add(lbl);
        row.Children.Add(bar);
        row.Children.Add(resetText);

        return (row, new AntigravityQuotaSlot(fill, resetText, window?.ResetsAt, isWeekly));
    }

    private AntigravityColumn BuildAntigravityGroupColumn(string group, List<AiUsageWindow> windows, string secondary)
    {
        var slots = new List<AntigravityQuotaSlot>();
        var rows = new StackPanel { Orientation = Orientation.Vertical, VerticalAlignment = VerticalAlignment.Center };

        var orderedWindows = windows
            .OrderBy(w => w.LabelKey == "AiUsageSession" ? 0 : w.LabelKey == "AiUsageWeekly" ? 1 : 2)
            .ToList();

        for (var i = 0; i < orderedWindows.Count; i++)
        {
            var win = orderedWindows[i];
            var isWeekly = win.LabelKey == "AiUsageWeekly" || (win.WindowSeconds is { } s && s >= 604800);
            var isLast = i == orderedWindows.Count - 1;
            var shortLabel = UiText.Get(isWeekly ? "AiUsageWeeklyShort" : "AiUsageSessionShort");
            var (row, slot) = CreateAntigravityBarRow(shortLabel, win, isWeekly, secondary, !isLast);
            rows.Children.Add(row);
            slots.Add(slot);
        }

        var header = Text(AntigravityGroupHeader(group), 9.5, secondary);
        header.FontWeight = FontWeights.SemiBold;
        header.HorizontalAlignment = HorizontalAlignment.Left;
        header.Margin = new Thickness(0, 0, 0, 1.5);
        Typography.SetNumeralAlignment(header, FontNumeralAlignment.Tabular);

        var colPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(2, 0, 2, 0)
        };
        colPanel.Children.Add(header);
        colPanel.Children.Add(rows);
        return new(colPanel, slots);
    }

    private AntigravityColumn BuildAntigravityPoolColumn(AiUsageWindow window, string secondary)
    {
        var slots = new List<AntigravityQuotaSlot>();
        var colPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(2, 0, 2, 0)
        };

        var label = AntigravityPoolHeader(window.LabelKey);
        var header = Text(label, 9.5, secondary);
        header.FontWeight = FontWeights.SemiBold;
        header.HorizontalAlignment = HorizontalAlignment.Left;
        header.Margin = new Thickness(0, 0, 0, 1.5);
        Typography.SetNumeralAlignment(header, FontNumeralAlignment.Tabular);
        colPanel.Children.Add(header);

        var track = new Border
        {
            Width = 32,
            Height = 4,
            CornerRadius = new CornerRadius(2),
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Left,
            Opacity = 0.35
        };
        track.SetResourceReference(Border.BackgroundProperty, "TextMuted");

        var used = window.Used;
        var fill = new Border
        {
            Height = 4,
            CornerRadius = new CornerRadius(2),
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Left,
            Width = Math.Max(0, Math.Min(32, 32 * used))
        };
        fill.SetResourceReference(Border.BackgroundProperty, Accent(used, window.Severity));

        var bar = new Grid
        {
            Width = 32,
            Height = 6,
            Margin = new Thickness(0, 0, 4, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        bar.Children.Add(track);
        bar.Children.Add(fill);

        string resetStr;
        var isWeekly = window.WindowSeconds is { } s && s >= 604800;
        if (window.ResetsAt is { } reset)
        {
            var rem = reset - DateTimeOffset.UtcNow;
            resetStr = !isWeekly && rem < RelativeLimit
                ? (rem > TimeSpan.Zero ? Duration(rem) : Duration(TimeSpan.Zero))
                : When(reset);
        }
        else
        {
            resetStr = "—";
        }

        var resetText = Text(resetStr, 8.5, secondary);
        resetText.VerticalAlignment = VerticalAlignment.Center;
        Typography.SetNumeralAlignment(resetText, FontNumeralAlignment.Tabular);

        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center
        };
        row.Children.Add(bar);
        row.Children.Add(resetText);
        colPanel.Children.Add(row);

        slots.Add(new AntigravityQuotaSlot(fill, resetText, window.ResetsAt, isWeekly));
        return new(colPanel, slots);
    }

    private UIElement BuildFlyout(List<(TextBlock Block, DateTimeOffset Reset)> countdowns)
    {
        countdowns.Clear();
        var now = DateTimeOffset.UtcNow;
        var root = new StackPanel { Width = 280 };
        root.Children.Add(SectionHeader(_provider, _report));
        if (_error is not null)
        {
            var message = Text(ErrorText(_provider, _error), 11.5, "TextSecondary", new Thickness(0, 0, 0, 8));
            message.TextWrapping = TextWrapping.Wrap;
            root.Children.Add(message);
        }
        if (_report is { } report)
        {
            var pace = PaceWindow(report, now);
            string? currentGroup = null;
            foreach (var window in report.Windows)
            {
                if (window.Group is { Length: > 0 } group && !string.Equals(group, currentGroup, StringComparison.Ordinal))
                {
                    var isFirstGroup = currentGroup is null;
                    currentGroup = group;
                    var groupHeader = Text(GroupLabel(group), 11.5, "TextPrimary",
                        new Thickness(0, isFirstGroup ? 0 : 4, 0, window.GroupModels is null ? 4 : 1));
                    groupHeader.FontWeight = FontWeights.SemiBold;
                    root.Children.Add(groupHeader);
                    if (window.GroupModels is { Length: > 0 } models)
                    {
                        var subtitle = Text(models, 10, "TextSecondary", new Thickness(0, 0, 0, 5));
                        subtitle.TextWrapping = TextWrapping.Wrap;
                        root.Children.Add(subtitle);
                    }
                }
                root.Children.Add(WindowRow(window, now, window == pace, countdowns));
            }
            if (report.Spend is { } spend) root.Children.Add(SpendRow(spend));
            if (report.Breakdown.Count > 0) root.Children.Add(BreakdownTiles(report.Breakdown));
            var extras = new List<string>();
            if (report.UnlimitedCredits) extras.Add(UiText.Get("AiUsageCreditsUnlimited"));
            else if (report.Credits is { } credits) extras.Add(UiText.Format("AiUsageCredits", credits.ToString("0.##", Culture)));
            if (report.ResetCredits > 0) extras.Add(UiText.Format("AiUsageResetCredits", report.ResetCredits.ToString(Culture)));
            if (extras.Count > 0) root.Children.Add(Text(string.Join(" · ", extras), 11, "TextSecondary", new Thickness(0, 0, 0, 6)));
        }
        root.Children.Add(Divider());
        root.Children.Add(Footer());
        return root;
    }

    private UIElement SectionHeader(AiProviders provider, AiUsageReport? report)
    {
        var (status, brush) = _error == "ErrorAiUsageSignIn" ? ("AiUsageStatusSignIn", "WidgetWarning")
            : report is not null && IsLimitReached(report) ? Status(report)
            : report is not null && IsStale(report) ? ("AiUsageStatusStale", "TextMuted")
            : report is not null ? Status(report)
            : _error is not null ? ("AiUsageStatusUnavailable", "TextMuted") : ("AiUsageStatusLoading", "TextMuted");
        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 8) };
        var open = IconButton("IconOpen", "AiUsageOpenDashboard", 12, () => OpenDashboard(provider));
        open.Width = open.Height = 22;
        open.Margin = new Thickness(6, -3, -3, -3);
        DockPanel.SetDock(open, Dock.Right);
        header.Children.Add(open);
        var state = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var dot = new Border { Width = 6, Height = 6, CornerRadius = new CornerRadius(3), Margin = new Thickness(0, 1, 5, 0) };
        dot.SetResourceReference(Border.BackgroundProperty, brush);
        state.Children.Add(dot);
        state.Children.Add(Text(UiText.Get(status), 11, "TextSecondary"));
        DockPanel.SetDock(state, Dock.Right);
        header.Children.Add(state);
        var title = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var name = Text(ProviderName(provider), 13, "TextPrimary");
        name.FontWeight = FontWeights.SemiBold;
        title.Children.Add(name);
        if (report?.Plan is { } plan)
        {
            var chip = new Border { CornerRadius = new CornerRadius(6), Padding = new Thickness(6, 0, 6, 1), Margin = new Thickness(6, 1, 0, 0),
                VerticalAlignment = VerticalAlignment.Center, Child = Text(plan, 10.5, "TextSecondary") };
            chip.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
            title.Children.Add(chip);
        }
        header.Children.Add(title);
        return header;
    }

    private UIElement WindowRow(AiUsageWindow window, DateTimeOffset now, bool showPace,
        List<(TextBlock Block, DateTimeOffset Reset)> countdowns)
    {
        var row = new StackPanel { Margin = new Thickness(0, 0, 0, 8) };
        row.Children.Add(ValueLine(Label(window), Percent(window.Used)));
        row.Children.Add(Bar(window.Used, Accent(window.Used, window.Severity), Elapsed(window, now)));
        if (window.ResetsAt is { } reset)
        {
            var resets = Text(ResetText(reset), 11, "TextSecondary");
            countdowns.Add((resets, reset));
            row.Children.Add(resets);
        }
        if (showPace && LimitIn(window, now) is { } limit)
            row.Children.Add(Text(limit < RelativeLimit ? UiText.Format("AiUsagePaceIn", Duration(limit))
                : UiText.Format("AiUsagePaceAt", When(now + limit)), 11, Accent(window.Used, window.Severity)));
        return row;
    }

    private UIElement SpendRow(AiUsageSpend spend)
    {
        var used = spend.Used01 ?? (spend.Limit is > 0m ? (double?)(double)(spend.Used / spend.Limit.Value) : null);
        var amount = spend.Used.ToString("N2", Culture) + (spend.Limit is { } limit ? " / " + limit.ToString("N2", Culture) : string.Empty)
            + " " + spend.Currency;
        var row = new StackPanel { Margin = new Thickness(0, 0, 0, 8) };
        row.Children.Add(ValueLine(UiText.Get("AiUsageExtra"), amount));
        if (used is { } value) row.Children.Add(Bar(Math.Clamp(value, 0, 1), Accent(value), null));
        return row;
    }

    private UIElement BreakdownTiles(IReadOnlyList<AiUsageShare> rows)
    {
        var panel = new StackPanel { Margin = new Thickness(0, 0, 0, 6) };
        var heading = Text(UiText.Get("AiUsageBreakdown"), 11, "TextSecondary", new Thickness(0, 0, 0, 4));
        heading.FontWeight = FontWeights.SemiBold;
        panel.Children.Add(heading);
        var grid = new UniformGrid { Columns = Math.Min(3, rows.Count), Margin = new Thickness(-2, 0, -2, 0) };
        foreach (var share in rows)
        {
            var tile = new Border { CornerRadius = new CornerRadius(8), Padding = new Thickness(8, 5, 8, 5), Margin = new Thickness(2) };
            tile.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
            var value = Text(Percent(share.Used), 12.5, "TextPrimary");
            value.FontWeight = FontWeights.SemiBold;
            Typography.SetNumeralAlignment(value, FontNumeralAlignment.Tabular);
            var label = Text(share.Name, 10.5, "TextSecondary");
            label.TextTrimming = TextTrimming.CharacterEllipsis;
            tile.Child = new StackPanel { Children = { label, value } };
            grid.Children.Add(tile);
        }
        panel.Children.Add(grid);
        return panel;
    }

    private UIElement Footer()
    {
        var footer = new DockPanel();
        var refresh = IconButton("IconRefresh", "AiUsageRefresh", 14, () =>
        {
            if (_error is null && (_lastLoadAt is not { } last || DateTimeOffset.UtcNow - last >= TimeSpan.FromSeconds(30)))
                _nextLoadAt = null;
            _ = LoadAsync();
        });
        refresh.VerticalAlignment = VerticalAlignment.Top;
        DockPanel.SetDock(refresh, Dock.Right);
        footer.Children.Add(refresh);
        var notes = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        if (_report is { } fetched)
            notes.Children.Add(Text(UiText.Format("AiUsageUpdated", fetched.FetchedAt.ToLocalTime().ToString("t", Culture)), 11, "TextSecondary"));
        var note = Text(UiText.Get("AiUsageUnofficialNote"), 10.5, "TextSecondary", new Thickness(0, 1, 0, 0));
        note.TextWrapping = TextWrapping.Wrap;
        notes.Children.Add(note);
        footer.Children.Add(notes);
        return footer;
    }

    private static UIElement ValueLine(string label, string value)
    {
        var line = new DockPanel();
        var number = Text(value, 12, "TextPrimary");
        number.FontWeight = FontWeights.SemiBold;
        Typography.SetNumeralAlignment(number, FontNumeralAlignment.Tabular);
        DockPanel.SetDock(number, Dock.Right);
        line.Children.Add(number);
        var name = Text(label, 12, "TextPrimary");
        name.TextTrimming = TextTrimming.CharacterEllipsis;
        line.Children.Add(name);
        return line;
    }

    /// <summary>A usage bar; the optional pace tick marks where usage would be at an even rate through the window.</summary>
    private static UIElement Bar(double value, string accent, double? pace)
    {
        var bar = new Grid { Height = 8, Margin = new Thickness(0, 3, 0, 2) };
        var track = new Border { Height = 4, CornerRadius = new CornerRadius(2) };
        track.SetResourceReference(Border.BackgroundProperty, "TextMuted");
        bar.Children.Add(track);
        var fill = new Border { Height = 4, CornerRadius = new CornerRadius(2) };
        fill.SetResourceReference(Border.BackgroundProperty, accent);
        bar.Children.Add(Split(value, fill, 0));
        if (pace is { } elapsed)
        {
            var tick = new Border { Width = 2, CornerRadius = new CornerRadius(1), HorizontalAlignment = HorizontalAlignment.Left,
                Margin = new Thickness(-1, 0, 0, 0) };
            tick.SetResourceReference(Border.BackgroundProperty, "TextPrimary");
            bar.Children.Add(Split(elapsed, tick, 1));
            bar.ToolTip = UiText.Get("AiUsagePaceMarker");
            bar.Background = Brushes.Transparent;
        }
        return bar;
    }

    private static Grid Split(double fraction, UIElement child, int column)
    {
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(fraction, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1 - fraction, GridUnitType.Star) });
        Grid.SetColumn(child, column);
        grid.Children.Add(child);
        return grid;
    }

    private static Border Divider()
    {
        var divider = new Border { Height = 1, Margin = new Thickness(0, 2, 0, 10) };
        divider.SetResourceReference(Border.BackgroundProperty, "MenuDivider");
        return divider;
    }

    private static TextBlock Text(string text, double size, string brush, Thickness margin = default)
    {
        var block = new TextBlock { Text = text, FontSize = size, Margin = margin, VerticalAlignment = VerticalAlignment.Center };
        block.SetResourceReference(TextBlock.ForegroundProperty, brush);
        return block;
    }

    private Button IconButton(string icon, string labelKey, double size, Action click)
    {
        var glyph = new ContentControl { Content = TryFindResource(icon), Width = size, Height = size };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextSecondary");
        var button = new Button { Content = glyph, ToolTip = UiText.Get(labelKey) };
        button.SetResourceReference(StyleProperty, "WidgetIconButton");
        AutomationProperties.SetName(button, UiText.Get(labelKey));
        button.Click += (_, _) => click();
        return button;
    }

    private static void OpenDashboard(AiProviders provider)
    {
        try
        {
            var url = provider switch
            {
                AiProviders.Claude => "https://claude.ai/settings/usage",
                AiProviders.Codex => "https://chatgpt.com/codex/settings/usage",
                AiProviders.OpenCode => "https://opencode.ai/go",
                AiProviders.Copilot => "https://github.com/settings/copilot",
                AiProviders.DeepSeek => "https://platform.deepseek.com/usage",
                AiProviders.OpenRouter => "https://openrouter.ai/settings/credits",
                _ => "https://antigravity.google/docs"
            };
            Process.Start(new ProcessStartInfo(url) { UseShellExecute = true });
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or InvalidOperationException)
        { AppLog.Info($"AI usage ({provider}) dashboard unavailable: {ex.GetType().Name}"); }
    }
}
