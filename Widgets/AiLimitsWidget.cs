using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Displays AI usage limits on the dock with concentric dual rings per pinned provider and an Aurora glass flyout.
/// </summary>
public sealed class AiLimitsWidget : DockWidget
{
    private const int MaxDockProviders = 1;
    private static readonly TimeSpan RefreshInterval = TimeSpan.FromMinutes(1);
    private static readonly TimeSpan RetryInterval = TimeSpan.FromSeconds(30);

    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) ClaudeBrushes =
        (FreezeBrush(Color.FromRgb(0xD9, 0x77, 0x57)), FreezeBrush(Color.FromRgb(0xF0, 0xA8, 0x8A)));
    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) CodexBrushes =
        (FreezeBrush(Color.FromRgb(0x71, 0x71, 0x7A)), FreezeBrush(Color.FromRgb(0xA1, 0xA1, 0xAA)));
    private static readonly SolidColorBrush CodexOnDockBrush = FreezeBrush(Color.FromRgb(0x33, 0x41, 0x55));
    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) AntigravityBrushes =
        (FreezeBrush(Color.FromRgb(0x42, 0x85, 0xF4)), FreezeBrush(Color.FromRgb(0x8A, 0xB4, 0xF8)));
    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) CopilotBrushes =
        (FreezeBrush(Color.FromRgb(0x8B, 0x5C, 0xF6)), FreezeBrush(Color.FromRgb(0xC4, 0xB5, 0xFD)));
    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) DeepSeekBrushes =
        (FreezeBrush(Color.FromRgb(0x4D, 0x6B, 0xFE)), FreezeBrush(Color.FromRgb(0x93, 0xC5, 0xFD)));
    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) OpenRouterBrushes =
        (FreezeBrush(Color.FromRgb(0x63, 0x66, 0xF1)), FreezeBrush(Color.FromRgb(0xA5, 0xB4, 0xFC)));
    private static readonly (SolidColorBrush Outer, SolidColorBrush Inner) DefaultPlatformBrushes =
        (FreezeBrush(Color.FromRgb(0x5E, 0xEA, 0xD4)), FreezeBrush(Color.FromRgb(0x38, 0xBD, 0xF8)));

    private static readonly Geometry ClaudeLogo = FreezeGeometry(
        "M12 2L13.5 8.8L18.6 4.2L14.8 10.1L21.5 9.2L15.4 12L21.5 14.8L14.8 13.9L18.6 19.8L13.5 15.2L12 22L10.5 15.2L5.4 19.8L9.2 13.9L2.5 14.8L8.6 12L2.5 9.2L9.2 10.1L5.4 4.2L10.5 8.8Z");
    private static readonly Geometry CodexLogo = FreezeGeometry(
        "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z");
    private static readonly Geometry AntigravityLogo = FreezeGeometry(
        "M12 2C12.7 7.4 16.6 11.3 22 12C16.6 12.7 12.7 16.6 12 22C11.3 16.6 7.4 12.7 2 12C7.4 11.3 11.3 7.4 12 2Z");
    private static readonly Geometry CopilotLogo = FreezeGeometry(
        "M12 3C7.5 3 4.5 5.6 4.5 9.5V11C3.1 11.4 2 12.6 2 14.2V16.8C2 18.6 3.4 20 5.2 20H7.5C8.5 20.6 10.1 21 12 21C13.9 21 15.5 20.6 16.5 20H18.8C20.6 20 22 18.6 22 16.8V14.2C22 12.6 20.9 11.4 19.5 11V9.5C19.5 5.6 16.5 3 12 3ZM9 12.5C9.8 12.5 10.5 13.2 10.5 14V16C10.5 16.8 9.8 17.5 9 17.5C8.2 17.5 7.5 16.8 7.5 16V14C7.5 13.2 8.2 12.5 9 12.5ZM15 12.5C15.8 12.5 16.5 13.2 16.5 14V16C16.5 16.8 15.8 17.5 15 17.5C14.2 17.5 13.5 16.8 13.5 16V14C13.5 13.2 14.2 12.5 15 12.5Z");
    private static readonly Geometry DeepSeekLogo = FreezeGeometry(
        "M12 3C7 3 3 7 3 12C3 17 7 21 12 21C16.4 21 20.1 17.9 20.9 13.7C19.6 15.1 17.6 15.8 15.5 15.5C13.1 15.1 11.3 13.1 11.3 10.6C11.3 8.1 13.1 6 15.5 5.6C14.4 3.9 13.3 3 12 3Z");
    private static readonly Geometry OpenRouterLogo = FreezeGeometry(
        "M4 7H14.5L17.5 4M4 17H14.5L17.5 20M4 12H20M17 9L20 12L17 15");

    private static Geometry FreezeGeometry(string pathData)
    {
        var geometry = Geometry.Parse(pathData);
        geometry.Freeze();
        return geometry;
    }

    private static Geometry? GetPlatformLogo(string? providerId) =>
        providerId switch
        {
            "claude" => ClaudeLogo,
            "codex" => CodexLogo,
            "antigravity" => AntigravityLogo,
            "copilot" => CopilotLogo,
            "deepseek" => DeepSeekLogo,
            "openrouter" => OpenRouterLogo,
            _ => null
        };

    private readonly StackPanel _dockContent;
    private readonly Button _card;
    private readonly DockFlyout _flyout = new();
    private readonly List<string> _pinnedIds;
    private ContentControl? _flyoutContentHost;
    private IReadOnlyList<AiProviderReport> _reports = [];
    private DateTime _loadedAt = DateTime.MinValue;
    private CancellationTokenSource? _loading;

    private static SolidColorBrush FreezeBrush(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    private static (SolidColorBrush Outer, SolidColorBrush Inner) GetPlatformBrushes(string? providerId) =>
        providerId switch
        {
            "claude" => ClaudeBrushes,
            "codex" => CodexBrushes,
            "antigravity" => AntigravityBrushes,
            "copilot" => CopilotBrushes,
            "deepseek" => DeepSeekBrushes,
            "openrouter" => OpenRouterBrushes,
            _ => DefaultPlatformBrushes
        };

    public AiLimitsWidget()
    {
        _pinnedIds = LoadPinnedProviders();
        _dockContent = new StackPanel { Orientation = Orientation.Horizontal };

        _card = new Button
        {
            Content = _dockContent,
            MinHeight = 36,
            VerticalAlignment = VerticalAlignment.Center,
            Padding = new Thickness(10, 5, 12, 5)
        };
        _card.SetResourceReference(StyleProperty, "WidgetCardButton");
        _card.Click += (_, _) => _flyout.Toggle(_card, BuildFlyout);

        Content = _card;
        HasData = true;
        Render();
    }

    protected override void OnLanguageChanged() => Render();

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        _ = LoadAsync();
    }

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        _loading?.Cancel();
        _flyout.Close();
    }

    private void UiText_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e) => Render();

    protected override void OnTick(DateTime now)
    {
        var hasReady = _reports.Any(r => r.Status == AiProviderStatus.Ready);
        var interval = hasReady ? RefreshInterval : RetryInterval;
        if (DateTime.UtcNow - _loadedAt > interval && _loading is null)
            _ = LoadAsync();
    }

    private async Task LoadAsync(bool interactive = false)
    {
        if (_loading is not null) return;
        var loading = new CancellationTokenSource();
        _loading = loading;
        _loadedAt = DateTime.UtcNow;

        try
        {
            var reports = await AiUsageService.LoadAllAsync(loading.Token, interactive);
            if (!ReferenceEquals(_loading, loading) || !IsActive) return;
            _loading = null;
            _reports = reports;
            EnsureDefaultPins();
            Render();
        }
        finally
        {
            if (ReferenceEquals(_loading, loading))
            {
                _loading = null;
                if (IsActive && _flyoutContentHost is not null)
                    _flyoutContentHost.Content = BuildFlyoutBody();
            }
            loading.Dispose();
        }
    }

    private void EnsureDefaultPins()
    {
        if (_pinnedIds.Count > 0) return;
        foreach (var report in _reports.Where(r => r.Status == AiProviderStatus.Ready).Take(MaxDockProviders))
            _pinnedIds.Add(report.ProviderId);
    }

    private void TogglePin(string providerId)
    {
        if (_pinnedIds.Count == 1 && string.Equals(_pinnedIds[0], providerId, StringComparison.Ordinal))
            return;

        _pinnedIds.Clear();
        _pinnedIds.Add(providerId);
        SavePinnedProviders(_pinnedIds);
        Render();
    }

    private void Render()
    {
        HasData = true;
        _dockContent.Children.Clear();

        var dockReports = _pinnedIds
            .Select(id => _reports.FirstOrDefault(r => r.ProviderId == id))
            .Where(r => r is { Status: AiProviderStatus.Ready, Windows.Count: > 0 })
            .Cast<AiProviderReport>()
            .Take(MaxDockProviders)
            .ToList();

        if (dockReports.Count == 0)
        {
            var firstReady = _reports.FirstOrDefault(r => r.Status == AiProviderStatus.Ready && r.Windows.Count > 0);
            if (firstReady is not null)
                dockReports.Add(firstReady);
        }

        if (dockReports.Count > 0)
        {
            for (var i = 0; i < dockReports.Count; i++)
            {
                if (i > 0)
                {
                    var sep = new Border
                    {
                        Width = 1,
                        Height = 20,
                        Margin = new Thickness(10, 0, 10, 0),
                        VerticalAlignment = VerticalAlignment.Center
                    };
                    sep.SetResourceReference(Border.BackgroundProperty, "Divider");
                    _dockContent.Children.Add(sep);
                }

                _dockContent.Children.Add(BuildDockProviderSegment(dockReports[i]));
            }

            var tooltip = string.Join(Environment.NewLine, dockReports.Select(r =>
                $"{r.DisplayName}: " + string.Join(" · ", r.Windows.Select(w =>
                    $"{UiText.Get(w.LabelKey)} {Math.Round(w.UtilizationPercent)}%"))));
            _card.ToolTip = tooltip;
            System.Windows.Automation.AutomationProperties.SetName(_card, $"{UiText.Get("WidgetAiLimits")}: {tooltip}");
        }
        else
        {
            var (outerBrand, innerBrand) = GetPlatformBrushes(null);
            var gauge = CreateSplitGauge(0, 0, outerBrand, innerBrand);
            var title = new TextBlock
            {
                Text = UiText.Get("WidgetAiLimits"),
                FontSize = 11.5,
                FontWeight = FontWeights.SemiBold
            };
            title.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

            var sub = new TextBlock
            {
                Text = UiText.Get("AiLimitsSetupShort"),
                FontSize = 11
            };
            sub.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");

            var stack = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
            stack.Children.Add(title);
            stack.Children.Add(sub);

            _dockContent.Children.Add(gauge);
            _dockContent.Children.Add(stack);

            _card.ToolTip = $"{UiText.Get("WidgetAiLimits")} · {UiText.Get("AiLimitsSetupShort")}";
            System.Windows.Automation.AutomationProperties.SetName(_card, UiText.Get("WidgetAiLimits"));
        }

        if (_flyoutContentHost is not null)
            _flyoutContentHost.Content = BuildFlyoutBody();
    }

    private UIElement BuildDockProviderSegment(AiProviderReport report)
    {
        var (outerBrand, innerBrand) = GetPlatformBrushes(report.ProviderId);
        var primaryVal = report.Windows[0].UtilizationPercent;
        var secondaryVal = report.Windows.Count > 1 ? report.Windows[1].UtilizationPercent : 0;

        var gauge = CreateSplitGauge(
            primaryVal,
            secondaryVal,
            outerBrand,
            innerBrand,
            GetPlatformLogo(report.ProviderId));

        var label = new TextBlock
        {
            Text = string.IsNullOrWhiteSpace(report.PlanLabel)
                ? report.DisplayName
                : $"{report.DisplayName} · {report.PlanLabel}",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold
        };
        label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

        var summary = new TextBlock
        {
            Text = string.Join(" | ", report.Windows.Select(FormatDockWindowSummary)),
            FontSize = 11.5
        };
        summary.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        System.Windows.Documents.Typography.SetNumeralAlignment(summary, FontNumeralAlignment.Tabular);

        var infoStack = new StackPanel
        {
            VerticalAlignment = VerticalAlignment.Center,
            MinWidth = 156
        };
        infoStack.Children.Add(label);
        infoStack.Children.Add(summary);

        var row = new StackPanel { Orientation = Orientation.Horizontal };
        row.Children.Add(gauge);
        row.Children.Add(infoStack);
        return row;
    }

    private static SplitArcGauge CreateSplitGauge(
        double topPercent,
        double bottomPercent,
        Brush topBrand,
        Brush bottomBrand,
        Geometry? logo = null)
    {
        var gauge = new SplitArcGauge
        {
            Width = 30,
            Height = 30,
            StrokeThickness = 2.8,
            TopValue = Math.Clamp(topPercent / 100.0, 0, 1),
            BottomValue = Math.Clamp(bottomPercent / 100.0, 0, 1),
            CenterGlyph = logo,
            Margin = new Thickness(0, 0, 9, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        gauge.SetResourceReference(SplitArcGauge.TrackBrushProperty, "WidgetTrack");
        if (logo is not null)
            gauge.CenterBrush = topBrand;
        else
            gauge.SetResourceReference(SplitArcGauge.CenterBrushProperty, "WidgetTrack");

        if (topPercent >= 90)
            gauge.SetResourceReference(SplitArcGauge.TopBrushProperty, "WidgetFocusAccent");
        else
            gauge.TopBrush = topBrand;

        if (bottomPercent >= 90)
            gauge.SetResourceReference(SplitArcGauge.BottomBrushProperty, "WidgetFocusAccent");
        else
            gauge.BottomBrush = bottomBrand;

        return gauge;
    }

    private string FormatDockWindowSummary(AiUsageWindow window)
    {
        var label = UiText.Get(window.LabelKey);
        if (!string.IsNullOrWhiteSpace(window.DisplayValue))
            return $"{label} {window.DisplayValue}";

        if (window.ResetsAt is not { } resetsAt)
            return $"{label} {Math.Round(window.UtilizationPercent)}%";

        var localReset = resetsAt.ToLocalTime();
        var remaining = resetsAt - DateTimeOffset.UtcNow;
        if (remaining <= TimeSpan.Zero)
            return $"{label} {UiText.Get("AiLimitsResetSoon")}";

        var resetPart = remaining.TotalHours < 24
            ? localReset.ToString("HH:mm", Settings.Language.Culture)
            : localReset.ToString("d MMM", Settings.Language.Culture);

        return $"{label} {resetPart}";
    }

    private UIElement BuildFlyout()
    {
        _flyoutContentHost = new ContentControl { Content = BuildFlyoutBody() };
        return _flyoutContentHost;
    }

    private UIElement BuildFlyoutBody()
    {
        var root = new StackPanel { Width = 344 };

        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 10) };
        var refreshBtn = new Button
        {
            Content = _loading is not null ? "..." : UiText.Get("AiLimitsRefresh"),
            FontSize = 11.5,
            Padding = new Thickness(8, 3, 8, 3),
            IsEnabled = _loading is null
        };
        refreshBtn.SetResourceReference(StyleProperty, "FlyoutButton");
        refreshBtn.Click += async (_, _) =>
        {
            refreshBtn.IsEnabled = false;
            refreshBtn.Content = "...";
            await LoadAsync(interactive: true);
        };
        DockPanel.SetDock(refreshBtn, Dock.Right);
        header.Children.Add(refreshBtn);

        var heading = DockFlyout.Heading(UiText.Get("WidgetAiLimits"));
        heading.Margin = new Thickness(0);
        heading.VerticalAlignment = VerticalAlignment.Center;
        header.Children.Add(heading);
        root.Children.Add(header);

        if (_reports.Count == 0)
        {
            root.Children.Add(DockFlyout.Caption(UiText.Get("AiLimitsLoading")));
            return root;
        }

        var activeReports = _reports
            .Where(r => r.Status != AiProviderStatus.NotConfigured)
            .OrderByDescending(r => _pinnedIds.Contains(r.ProviderId, StringComparer.Ordinal))
            .ThenBy(r => r.Status == AiProviderStatus.Ready ? 0 : 1)
            .ToList();
        var unconfiguredReports = _reports.Where(r => r.Status == AiProviderStatus.NotConfigured).ToList();

        for (var i = 0; i < activeReports.Count; i++)
        {
            if (i > 0)
                root.Children.Add(BuildDivider());

            root.Children.Add(BuildProviderCard(activeReports[i]));
        }

        if (unconfiguredReports.Count > 0)
        {
            if (activeReports.Count > 0)
                root.Children.Add(BuildDivider());

            var moreHeading = new TextBlock
            {
                Text = UiText.Get("AiLimitsMoreProviders"),
                FontSize = 11,
                FontWeight = FontWeights.SemiBold,
                Margin = new Thickness(0, 0, 0, 6)
            };
            moreHeading.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            root.Children.Add(moreHeading);

            for (var i = 0; i < unconfiguredReports.Count; i++)
            {
                if (i > 0)
                    root.Children.Add(BuildDivider(compact: true));

                root.Children.Add(BuildCompactUnconfiguredRow(unconfiguredReports[i]));
            }
        }

        var latestFetch = _reports.Max(r => r.FetchedAt).ToLocalTime();
        var updatedText = new TextBlock
        {
            Text = UiText.Format("AiLimitsUpdatedAt", latestFetch.ToString("HH:mm", Settings.Language.Culture)),
            FontSize = 11,
            Margin = new Thickness(2, 6, 0, 0)
        };
        updatedText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        root.Children.Add(updatedText);

        return root;
    }

    private UIElement BuildProviderCard(AiProviderReport report)
    {
        var (outerBrand, innerBrand) = GetPlatformBrushes(report.ProviderId);
        var isPinned = _pinnedIds.Contains(report.ProviderId, StringComparer.Ordinal);

        var stack = new StackPanel
        {
            Margin = new Thickness(0, 4, 0, 4)
        };
        var topRow = new DockPanel { Margin = new Thickness(0, 0, 0, 10) };

        if (report.Status == AiProviderStatus.Ready)
        {
            var pinBtn = new Button
            {
                Content = UiText.Get(isPinned ? "AiLimitsOnDock" : "AiLimitsPinToDock"),
                FontSize = 11,
                FontWeight = isPinned ? FontWeights.SemiBold : FontWeights.Normal,
                Padding = new Thickness(10, 4, 10, 4),
                VerticalAlignment = VerticalAlignment.Center
            };
            pinBtn.SetResourceReference(StyleProperty, "FlyoutButton");
            if (isPinned)
            {
                pinBtn.Background = report.ProviderId == "codex" ? CodexOnDockBrush : outerBrand;
                pinBtn.Foreground = Brushes.White;
            }
            pinBtn.Click += (_, _) => TogglePin(report.ProviderId);
            DockPanel.SetDock(pinBtn, Dock.Right);
            topRow.Children.Add(pinBtn);
        }

        var titleStack = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };

        var logo = GetPlatformLogo(report.ProviderId);
        if (logo is not null)
        {
            var logoPath = new System.Windows.Shapes.Path
            {
                Data = logo,
                Width = 18,
                Height = 18,
                Stretch = Stretch.Uniform,
                Fill = outerBrand,
                Margin = new Thickness(0, 0, 8, 0),
                VerticalAlignment = VerticalAlignment.Center
            };
            titleStack.Children.Add(logoPath);
        }
        else
        {
            var brandDot = new Border
            {
                Width = 8,
                Height = 8,
                CornerRadius = new CornerRadius(4),
                Background = outerBrand,
                Margin = new Thickness(0, 0, 7, 0),
                VerticalAlignment = VerticalAlignment.Center
            };
            titleStack.Children.Add(brandDot);
        }

        var nameText = new TextBlock
        {
            Text = report.DisplayName,
            FontSize = 14,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        nameText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        titleStack.Children.Add(nameText);

        if (!string.IsNullOrWhiteSpace(report.PlanLabel))
        {
            var badge = new Border
            {
                CornerRadius = new CornerRadius(5),
                Padding = new Thickness(6, 1, 6, 2),
                Margin = new Thickness(8, 0, 0, 0),
                VerticalAlignment = VerticalAlignment.Center
            };
            badge.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");
            var badgeText = new TextBlock
            {
                Text = report.PlanLabel,
                FontSize = 10.5,
                FontWeight = FontWeights.SemiBold
            };
            badgeText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            badge.Child = badgeText;
            titleStack.Children.Add(badge);
        }

        topRow.Children.Add(titleStack);
        stack.Children.Add(topRow);

        if (report.Status == AiProviderStatus.Ready && report.Windows.Count > 0)
        {
            for (var i = 0; i < report.Windows.Count; i++)
            {
                var windowBrush = i == 0 ? outerBrand : innerBrand;
                stack.Children.Add(BuildWindowRow(report.Windows[i], windowBrush, isLast: i == report.Windows.Count - 1));
            }
        }
        else
        {
            var message = new TextBlock
            {
                Text = UiText.Get(report.ErrorKey ?? "AiLimitsLoadError"),
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
                Margin = new Thickness(0, 0, 0, report.SetupCommand is not null ? 8 : 0)
            };
            message.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            stack.Children.Add(message);

            if (!string.IsNullOrWhiteSpace(report.SetupCommand))
            {
                var cmdBtn = new Button
                {
                    Content = $"> {report.SetupCommand}   ({UiText.Get("AiLimitsCopyCommand")})",
                    FontFamily = new FontFamily("Consolas"),
                    FontSize = 11.5,
                    Padding = new Thickness(10, 6, 10, 6),
                    HorizontalContentAlignment = HorizontalAlignment.Left
                };
                cmdBtn.SetResourceReference(StyleProperty, "FlyoutButton");
                cmdBtn.Click += (_, _) =>
                {
                    try
                    {
                        Clipboard.SetText(report.SetupCommand);
                        cmdBtn.Content = $"> {report.SetupCommand}   ({UiText.Get("AiLimitsCopied")})";
                    }
                    catch (System.Runtime.InteropServices.ExternalException)
                    {
                        // Clipboard may be temporarily locked by another process.
                    }
                };
                stack.Children.Add(cmdBtn);
            }
        }

        return stack;
    }

    private static Border BuildDivider(bool compact = false)
    {
        var divider = new Border
        {
            Height = 1,
            Margin = compact ? new Thickness(0, 5, 0, 5) : new Thickness(0, 10, 0, 10)
        };
        divider.SetResourceReference(Border.BackgroundProperty, "MenuDivider");
        return divider;
    }

    private static UIElement BuildCompactUnconfiguredRow(AiProviderReport report)
    {
        var (outerBrand, _) = GetPlatformBrushes(report.ProviderId);
        var container = new StackPanel
        {
            Margin = new Thickness(0, 3, 0, 3)
        };

        var topRow = new DockPanel();

        var statusLabel = new TextBlock
        {
            Text = report.SetupCommand is not null ? $"> {report.SetupCommand}" : UiText.Get("AiLimitsNotConnected"),
            FontSize = 11,
            VerticalAlignment = VerticalAlignment.Center
        };
        statusLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(statusLabel, Dock.Right);
        topRow.Children.Add(statusLabel);

        var left = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var logo = GetPlatformLogo(report.ProviderId);
        if (logo is not null)
        {
            left.Children.Add(new System.Windows.Shapes.Path
            {
                Data = logo,
                Width = 14,
                Height = 14,
                Stretch = Stretch.Uniform,
                Fill = outerBrand,
                Opacity = 0.75,
                Margin = new Thickness(0, 0, 8, 0),
                VerticalAlignment = VerticalAlignment.Center
            });
        }

        var name = new TextBlock
        {
            Text = report.DisplayName,
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        name.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        left.Children.Add(name);
        topRow.Children.Add(left);
        container.Children.Add(topRow);

        var footnote = new TextBlock
        {
            Text = UiText.Get(report.ErrorKey ?? "AiLimitsSetupShort"),
            FontSize = 10.5,
            Opacity = 0.8,
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(logo is not null ? 22 : 0, 2, 0, 0)
        };
        footnote.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        container.Children.Add(footnote);

        return container;
    }

    private UIElement BuildWindowRow(AiUsageWindow window, Brush brandBrush, bool isLast)
    {
        if (!string.IsNullOrWhiteSpace(window.DisplayValue))
        {
            var valueRow = new DockPanel
            {
                Margin = new Thickness(0, 0, 0, isLast ? 0 : 7)
            };

            var valText = new TextBlock
            {
                Text = window.DisplayValue,
                FontSize = 12.5,
                FontWeight = FontWeights.SemiBold,
                VerticalAlignment = VerticalAlignment.Center
            };
            valText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            System.Windows.Documents.Typography.SetNumeralAlignment(valText, FontNumeralAlignment.Tabular);
            DockPanel.SetDock(valText, Dock.Right);
            valueRow.Children.Add(valText);

            var keyText = new TextBlock
            {
                Text = UiText.Get(window.LabelKey),
                FontSize = 12,
                VerticalAlignment = VerticalAlignment.Center
            };
            keyText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            valueRow.Children.Add(keyText);
            return valueRow;
        }

        var row = new Grid
        {
            Margin = new Thickness(0, 0, 0, isLast ? 0 : 7)
        };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(68) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(56) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(40) });

        var labelText = new TextBlock
        {
            Text = UiText.Get(window.LabelKey),
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center
        };
        labelText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        Grid.SetColumn(labelText, 0);
        row.Children.Add(labelText);

        var clamped = Math.Clamp(window.UtilizationPercent, 0, 100);
        var track = new Border
        {
            Height = 4,
            CornerRadius = new CornerRadius(2),
            ClipToBounds = true,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(6, 0, 10, 0),
            Child = BuildProgressFill(clamped, brandBrush, 2)
        };
        track.SetResourceReference(Border.BackgroundProperty, "WidgetTrack");
        Grid.SetColumn(track, 1);
        row.Children.Add(track);

        if (window.ResetsAt is { } resetsAt)
        {
            var resetText = new TextBlock
            {
                Text = FormatResetTime(resetsAt),
                FontSize = 11.5,
                HorizontalAlignment = HorizontalAlignment.Right,
                VerticalAlignment = VerticalAlignment.Center
            };
            resetText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            Grid.SetColumn(resetText, 2);
            row.Children.Add(resetText);
        }

        var percentText = new TextBlock
        {
            Text = $"{Math.Round(window.UtilizationPercent).ToString(Settings.Language.Culture)}%",
            FontSize = 12.5,
            FontWeight = FontWeights.SemiBold,
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center
        };
        percentText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        System.Windows.Documents.Typography.SetNumeralAlignment(percentText, FontNumeralAlignment.Tabular);
        Grid.SetColumn(percentText, 3);
        row.Children.Add(percentText);

        return row;
    }

    private static UIElement? BuildProgressFill(double percent, Brush brandBrush, double radius)
    {
        var clamped = Math.Clamp(percent, 0, 100);
        if (clamped <= 0.1) return null;

        var barGrid = new Grid();
        barGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(clamped, GridUnitType.Star) });
        barGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(Math.Max(0, 100 - clamped), GridUnitType.Star) });

        var fill = new Border { CornerRadius = new CornerRadius(radius) };
        if (clamped >= 90)
            fill.SetResourceReference(Border.BackgroundProperty, "WidgetFocusAccent");
        else
            fill.Background = brandBrush;

        Grid.SetColumn(fill, 0);
        barGrid.Children.Add(fill);
        return barGrid;
    }

    private string FormatResetTime(DateTimeOffset resetsAt)
    {
        var remaining = resetsAt - DateTimeOffset.UtcNow;
        if (remaining <= TimeSpan.Zero)
            return UiText.Get("AiLimitsResetSoon");

        if (remaining.TotalHours < 24)
            return $"{(int)remaining.TotalHours}h {remaining.Minutes}m";

        return resetsAt.ToLocalTime().ToString("ddd HH:mm", Settings.Language.Culture);
    }

    private static string GetPinsFilePath() =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "OpenTaskbar", "ai-limits-pins.json");

    private static List<string> LoadPinnedProviders()
    {
        try
        {
            var path = GetPinsFilePath();
            if (!File.Exists(path)) return [];
            var items = JsonSerializer.Deserialize<List<string>>(File.ReadAllText(path));
            return items?.Take(MaxDockProviders).ToList() ?? [];
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            return [];
        }
    }

    private static void SavePinnedProviders(IReadOnlyList<string> ids)
    {
        try
        {
            var path = GetPinsFilePath();
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, JsonSerializer.Serialize(ids));
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            // Ignore non-critical pin persistence errors.
        }
    }
}
