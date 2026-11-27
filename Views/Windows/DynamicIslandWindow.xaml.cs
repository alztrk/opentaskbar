using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;
using OpenTaskbar.Widgets;
using LiveDockWidget = OpenTaskbar.Widgets.DockWidget;

namespace OpenTaskbar.Views.Windows;

public partial class DynamicIslandWindow : Window, IDisposable
{
    private const double CompactHeight = 50;
    private const double ExpandedHeight = 246;
    private const double CompactSurfaceWidth = 320;
    private const double ExpandedSurfaceWidth = 416;
    private const double MinimumSurfaceWidth = 240;
    private const double ShoulderWidth = 12;
    private const long WsExToolWindow = 0x00000080;
    private const long WsExNoActivate = 0x08000000;
    private const long WsExAppWindow = 0x00040000;
    private const int WmMouseActivate = 0x0021;
    private const int MaNoActivate = 3;
    private const double RevealStripHeight = 4;
    private static readonly TimeSpan RevealCheckInterval = TimeSpan.FromMilliseconds(50);
    private static readonly TimeSpan SlideDuration = TimeSpan.FromMilliseconds(190);
    private static readonly TimeSpan ResizeDuration = TimeSpan.FromMilliseconds(180);

    private readonly WidgetCoordinator _widgetCoordinator;
    private readonly Action<DockSettings> _saveSettings;
    private readonly Action<string> _removeWidget;
    private readonly Func<string, IEnumerable<Control>> _createPlacementMenuItems;
    private readonly DispatcherTimer _visibilityTimer = new() { Interval = TimeSpan.FromMilliseconds(400) };
    private readonly DispatcherTimer _revealTimer = new() { Interval = RevealCheckInterval };
    private readonly HashSet<string> _placementIds = new(StringComparer.OrdinalIgnoreCase);
    private IReadOnlyDictionary<string, DockWidgetItem> _timedWidgets = new Dictionary<string, DockWidgetItem>();
    private IReadOnlyList<string> _pageIds = [];
    private DisplayInfo _display;
    private DockSettings _settings = new();
    private UIElement? _details;
    private Point _pointerOrigin;
    private double _dragOffset;
    private int _currentPageIndex;
    private int _dragStartIndex;
    private bool _isDragging;
    private bool _pointerGestureActive;
    private bool _isExpanded;
    private bool _syncingPages;
    private bool _resourcesDisposed;
    private bool _hiddenByAutoHide;

    public DynamicIslandWindow(
        DisplayInfo display,
        Action<string> reportError,
        Action<DockSettings> saveSettings,
        Action<string> removeWidget,
        Func<string, IEnumerable<Control>> createPlacementMenuItems)
    {
        InitializeComponent();
        PageViewport.SizeChanged += PageViewport_SizeChanged;
        IslandContent.SizeChanged += IslandContent_SizeChanged;
        Header.LostMouseCapture += Header_LostMouseCapture;
        _display = display;
        _saveSettings = saveSettings;
        _removeWidget = removeWidget;
        _createPlacementMenuItems = createPlacementMenuItems;

        _widgetCoordinator = new WidgetCoordinator(Pages, Dispatcher, reportError, lazy: true)
        {
            ExternalLayout = true,
            AllowWidgetReorder = false,
            EnableWidgetHover = false,
            IsWidgetEnabled = IsAssignedHere,
            CreateWidgetAdditionalMenuItems = CreateIslandAdditionalMenuItems,
            WidgetSettingsUpdated = _saveSettings
        };
        _widgetCoordinator.WidgetRemoveRequested += _removeWidget;
        _widgetCoordinator.AvailabilityChanged += SyncPages;
        _widgetCoordinator.WidgetsPlaced += SyncPages;
        InputManager.Current.PreProcessInput += InputManager_PreProcessInput;
        _visibilityTimer.Tick += VisibilityTimer_Tick;
        _revealTimer.Tick += RevealTimer_Tick;
        _visibilityTimer.Start();
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        UpdateControlLabels();
        PositionWindow(CompactSurfaceWidth, CompactHeight, animate: false);
    }

    public string DeviceName => _display.DeviceName;

    public void Apply(
        DisplayInfo display,
        IReadOnlyList<string> widgetIds,
        IReadOnlyDictionary<string, DockWidgetItem> timedWidgets,
        DockSettings settings)
    {
        if (_resourcesDisposed) return;
        _display = display;
        _settings = settings;
        _placementIds.Clear();
        foreach (var id in widgetIds) _placementIds.Add(id);
        _timedWidgets = timedWidgets;
        _widgetCoordinator.Apply(settings);
        SyncPages();
        PositionWindow(WidthForCurrentPage(), _isExpanded ? ExpandedHeight : CompactHeight, animate: false);
        RefreshVisibility();
    }

    public void Dispose()
    {
        if (_resourcesDisposed) return;
        ReleaseResources();
        if (IsVisible || new WindowInteropHelper(this).Handle != 0)
        {
            try { Close(); }
            catch (InvalidOperationException) { }
        }
    }

    private bool IsAssignedHere(DockSettings settings, string id) =>
        _placementIds.Contains(id)
        && DynamicIslandPolicy.IsAssignedToDisplay(
            settings.DynamicIslandPlacements, id, _display.DeviceName);

    private void SyncPages()
    {
        if (_resourcesDisposed || _syncingPages) return;
        _syncingPages = true;
        try
        {
            var available = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var pair in _widgetCoordinator.ActiveWidgets)
            {
                if (pair.Value.Visibility == Visibility.Visible) available.Add(pair.Key);
            }

            if (!_settings.FocusModeEnabled)
            {
                foreach (var pair in _timedWidgets)
                {
                    if (_placementIds.Contains(pair.Key)
                        && _settings.Widgets?.Contains(pair.Key, StringComparer.OrdinalIgnoreCase) == true)
                        available.Add(pair.Key);
                }
            }

            var displayOrder = _settings.DynamicIslandWidgetOrders?.TryGetValue(_display.DeviceName, out var savedOrder) == true
                ? savedOrder
                : null;
            var nextPageIds = DynamicIslandPolicy.OrderWidgets(
                available,
                displayOrder,
                _settings.WidgetOrder,
                WidgetCoordinator.DefaultWidgetOrder,
                _timedWidgets.Keys);
            if (_pageIds.SequenceEqual(nextPageIds, StringComparer.OrdinalIgnoreCase))
            {
                UpdatePageControls();
                return;
            }

            var selectedId = _pageIds.Count > 0 && _pageIds[_currentPageIndex] is { } selected
                ? selected
                : nextPageIds.FirstOrDefault();
            DetailsContent.Content = null;
            _details = null;
            DetailsPanel.Visibility = Visibility.Collapsed;
            foreach (var activeWidget in _widgetCoordinator.ActiveWidgets.Values)
            {
                DetachFromParent(activeWidget);
            }
            Pages.Children.Clear();
            foreach (var id in nextPageIds)
            {
                if (_widgetCoordinator.ActiveWidgets.TryGetValue(id, out var widget))
                {
                    widget.SetDynamicIslandState(true);
                    widget.SetCompactState(true);
                    widget.SetHoverExpanded(false);
                    Pages.Children.Add(CreateWidgetPage(id, widget));
                }
                else if (_timedWidgets.TryGetValue(id, out var timedWidget))
                {
                    var tile = new TimedWidgetTile
                    {
                        DataContext = timedWidget,
                        Title = GetIslandLabel(id),
                        IsDynamicIsland = true,
                        ContextMenu = CreateTimedContextMenu(id)
                    };
                    ApplyWidgetPalette(tile, id);
                    tile.AddHandler(TimedWidgetTile.DetailsRequestedEvent,
                        new RoutedEventHandler(TimedWidget_DetailsRequested));
                    Pages.Children.Add(tile);
                }
            }

            _pageIds = nextPageIds;
            var newIndex = selectedId is null ? 0 : nextPageIds.ToList().FindIndex(
                id => string.Equals(id, selectedId, StringComparison.OrdinalIgnoreCase));
            SetCurrentPageIndex(newIndex < 0 ? 0 : newIndex, animate: false);
            if (_isExpanded && !HasExpandableContent(CurrentPageId())) SetExpanded(false, animate: false);
            if (_pageIds.Count == 0)
            {
                _details = null;
                DetailsContent.Content = null;
                DetailsPanel.Visibility = Visibility.Collapsed;
                _isExpanded = false;
                Header.Cursor = Cursors.Arrow;
                UpdatePageControls();
                PositionWindow(CompactSurfaceWidth, CompactHeight, animate: false);
                RefreshVisibility();
                return;
            }

            Header.Cursor = Cursors.Hand;
            UpdatePageControls();
            PositionWindow(WidthForCurrentPage(), _isExpanded ? ExpandedHeight : CompactHeight, animate: false);
            RefreshVisibility();
        }
        finally
        {
            _syncingPages = false;
        }
    }

    private ContextMenu CreateTimedContextMenu(string id)
    {
        var menu = new ContextMenu();
        foreach (var item in _createPlacementMenuItems(id)) menu.Items.Add(item);
        if (menu.Items.Count > 0) menu.Items.Add(new Separator());
        menu.Items.Add(CreateMoveOrderMenuItem(id, -1));
        menu.Items.Add(CreateMoveOrderMenuItem(id, 1));
        menu.Items.Add(new Separator());
        var remove = new MenuItem { Header = UiText.Get("RemoveWidget") };
        if (Application.Current?.TryFindResource("IconTrash") is Geometry icon) remove.Icon = icon;
        remove.Click += (_, _) => _removeWidget(id);
        menu.Items.Add(remove);
        return menu;
    }

    private IEnumerable<Control> CreateIslandAdditionalMenuItems(string id)
    {
        var items = _createPlacementMenuItems(id).ToList();
        items.Add(CreateMoveOrderMenuItem(id, -1));
        items.Add(CreateMoveOrderMenuItem(id, 1));
        return items;
    }

    private MenuItem CreateMoveOrderMenuItem(string id, int offset)
    {
        var item = new MenuItem
        {
            Header = UiText.Get(offset < 0 ? "DynamicIslandMoveEarlier" : "DynamicIslandMoveLater")
        };
        item.Click += (_, _) => MoveWidgetInIsland(id, offset);
        return item;
    }

    private void MoveWidgetInIsland(string id, int offset)
    {
        var order = DynamicIslandPolicy.MoveWidget(_pageIds, id, offset);
        if (_pageIds.SequenceEqual(order, StringComparer.OrdinalIgnoreCase)) return;

        var orders = (_settings.DynamicIslandWidgetOrders ?? new Dictionary<string, IReadOnlyList<string>>())
            .ToDictionary(pair => pair.Key, pair => pair.Value, StringComparer.OrdinalIgnoreCase);
        orders[_display.DeviceName] = order;
        _settings = _settings with { DynamicIslandWidgetOrders = orders };
        _saveSettings(_settings);
    }

    private void TimedWidget_DetailsRequested(object sender, RoutedEventArgs e)
    {
        if (e.OriginalSource is not TimedWidgetTile { DataContext: DockWidgetItem widget }) return;
        e.Handled = true;
        ShowDetails(new TimedWidgetPanel { DataContext = widget }, widget.Id);
    }

    private void UpdatePageControls()
    {
        if (_pageIds.Count == 0)
        {
            PageIndicators.Children.Clear();
            PageNavigation.Visibility = Visibility.Collapsed;
            AutomationProperties.SetName(Header, UiText.Get("DynamicIslandTitle"));
            return;
        }

        PageNavigation.Visibility = _isExpanded && _pageIds.Count > 1
            ? Visibility.Visible
            : Visibility.Collapsed;
        PageIndicators.Children.Clear();
        for (var index = 0; index < _pageIds.Count; index++)
        {
            var indicator = new Border
            {
                Width = index == _currentPageIndex ? 18 : 7,
                Height = 3,
                CornerRadius = new CornerRadius(1.5),
                Margin = new Thickness(2, 0, 2, 0),
                IsHitTestVisible = false
            };
            indicator.SetResourceReference(Border.BackgroundProperty,
                index == _currentPageIndex ? "AccentBrush" : "StrokeSubtle");
            PageIndicators.Children.Add(indicator);
        }
        AutomationProperties.SetName(Header, UiText.Get(_isExpanded ? "DynamicIslandCollapse" : "DynamicIslandExpand"));
        AutomationProperties.SetHelpText(Header, GetIslandLabel(CurrentPageId() ?? string.Empty));
    }

    private bool HasExpandableContent(string? id) => id is not null
        && (_timedWidgets.ContainsKey(id) || _widgetCoordinator.ActiveWidgets.ContainsKey(id));

    private UIElement GetDetails(LiveDockWidget widget, string id)
    {
        var details = widget.BuildDynamicIslandExpandedContent();
        if (details is not null) return ApplyWidgetAccent(details, id);

        var label = DockWidgetService.TryGetDescriptor(id, out var descriptor)
            ? UiText.Get(descriptor.LabelKey)
            : id;
        var summary = AutomationProperties.GetName(widget);
        if (string.IsNullOrWhiteSpace(summary)) summary = UiText.Get("SystemDataUnavailable");
        return ApplyWidgetAccent(
            DynamicIslandStatusPanel.Create(label, summary, string.Empty, null, AccentFor(id)), id);
    }

    private static void DetachFromParent(UIElement element)
    {
        switch (LogicalTreeHelper.GetParent(element))
        {
            case Decorator decorator when ReferenceEquals(decorator.Child, element):
                decorator.Child = null;
                break;
            case ContentControl content when ReferenceEquals(content.Content, element):
                content.Content = null;
                break;
            case Panel panel:
                panel.Children.Remove(element);
                break;
        }
    }

    private Grid CreateWidgetPage(string id, LiveDockWidget widget)
    {
        DetachFromParent(widget);
        var page = new Grid
        {
            Width = Math.Max(1, PageViewport.ActualWidth),
            Height = CompactHeight,
            Background = Brushes.Transparent,
            ClipToBounds = true,
            Tag = id
        };
        ApplyWidgetPalette(page, id);
        var showIdentity = !id.Equals("Media", StringComparison.OrdinalIgnoreCase)
            && !id.Equals(DockWidgetService.FocusId, StringComparison.OrdinalIgnoreCase);
        page.ColumnDefinitions.Add(new ColumnDefinition
        {
            Width = showIdentity ? GridLength.Auto : new GridLength(0)
        });
        page.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        if (showIdentity)
        {
            var accentBrush = (Brush)page.Resources["WidgetAccent"];
            var identity = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(7, 0, 8, 0),
                MaxWidth = 112
            };
            var icon = new ContentControl
            {
                Width = 17,
                Height = 17,
                VerticalAlignment = VerticalAlignment.Center,
                Foreground = accentBrush,
                Content = DockWidgetService.TryGetDescriptor(id, out var descriptor)
                    ? Application.Current?.TryFindResource(descriptor.IconKey)
                    : null
            };
            icon.SetResourceReference(StyleProperty, id.StartsWith("AiUsage", StringComparison.OrdinalIgnoreCase)
                ? "WidgetGlyph"
                : "WidgetIcon");
            identity.Children.Add(icon);
            var label = new TextBlock
            {
                Text = GetIslandLabel(id),
                Margin = new Thickness(7, 0, 0, 0),
                MaxWidth = 82,
                VerticalAlignment = VerticalAlignment.Center,
                FontSize = 12.5,
                FontWeight = FontWeights.SemiBold,
                Foreground = Brushes.White,
                TextTrimming = TextTrimming.CharacterEllipsis
            };
            identity.Children.Add(label);
            page.Children.Add(identity);
        }

        var content = new Border
        {
            Child = widget,
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center,
            MaxWidth = showIdentity ? 166 : double.PositiveInfinity,
            Margin = new Thickness(0, 0, 7, 0),
            ClipToBounds = true
        };
        Grid.SetColumn(content, 1);
        page.Children.Add(content);
        return page;
    }

    private static UIElement ApplyWidgetAccent(UIElement content, string id)
    {
        var host = new Border { Child = content, Background = Brushes.Transparent };
        ApplyWidgetPalette(host, id);
        return host;
    }

    private static void ApplyWidgetPalette(FrameworkElement host, string id)
    {
        var accent = AccentFor(id);
        var accentBrush = new SolidColorBrush(accent);
        var accentSoft = new SolidColorBrush(Color.FromArgb(0x33, accent.R, accent.G, accent.B));
        var accentStroke = new SolidColorBrush(Color.FromArgb(0x80, accent.R, accent.G, accent.B));
        host.Resources["WidgetAccent"] = accentBrush;
        host.Resources["WidgetAccentFill"] = accentBrush;
        host.Resources["WidgetSelectedForeground"] = accentBrush;
        host.Resources["WidgetSelectedFill"] = accentSoft;
        host.Resources["WidgetSelectedStroke"] = accentStroke;
        host.Resources["WidgetOnStroke"] = accentStroke;
        host.Resources["WidgetBlue"] = accentBrush;
        host.Resources["WidgetGreen"] = accentBrush;
    }

    private static Color AccentFor(string id) => id switch
    {
        "Weather" => Color.FromRgb(0xFF, 0xD1, 0x97),
        "DayProgress" => Color.FromRgb(0x91, 0xDE, 0xDC),
        "Stopwatch" => Color.FromRgb(0xB1, 0xC9, 0xFF),
        "Countdown" => Color.FromRgb(0xFF, 0xCE, 0x96),
        "SystemStats" => Color.FromRgb(0xA8, 0xC6, 0xFF),
        "NetSpeed" => Color.FromRgb(0x94, 0xDF, 0xEC),
        "Battery" => Color.FromRgb(0xA7, 0xE5, 0xBA),
        "BluetoothBattery" => Color.FromRgb(0xAD, 0xCB, 0xFF),
        "Storage" => Color.FromRgb(0xBB, 0xD3, 0xFA),
        "Media" => Color.FromRgb(0xE0, 0xBD, 0xFF),
        "Audio" => Color.FromRgb(0xC4, 0xBA, 0xFF),
        "QuickSettings" => Color.FromRgb(0xB7, 0xD3, 0xFF),
        "Focus" => Color.FromRgb(0xD1, 0xC2, 0xFF),
        "AiUsageClaude" => Color.FromRgb(0xF2, 0xB8, 0x97),
        "AiUsageCodex" => Color.FromRgb(0xB9, 0xE1, 0xCF),
        "AiUsageOpenCode" => Color.FromRgb(0xEF, 0xE1, 0xB8),
        "AiUsageCopilot" => Color.FromRgb(0xAC, 0xDA, 0xF8),
        "AiUsageDeepSeek" => Color.FromRgb(0xA9, 0xBA, 0xFF),
        "AiUsageOpenRouter" => Color.FromRgb(0xD3, 0xBC, 0xEF),
        "AiUsageCursor" => Color.FromRgb(0xE1, 0xE6, 0xEF),
        _ => Color.FromRgb(0xC2, 0xD9, 0xFF)
    };

    private static string GetIslandLabel(string id)
    {
        if (id.StartsWith("AiUsage", StringComparison.OrdinalIgnoreCase))
            return id["AiUsage".Length..] switch
            {
                "OpenCode" => "OpenCode",
                "Copilot" => "Copilot",
                var provider => provider
            };

        var shortLabelKey = id switch
        {
            "Clock" => "DynamicIslandShortClock",
            "Weather" => "DynamicIslandShortWeather",
            "DayProgress" => "DynamicIslandShortDay",
            "Stopwatch" => "DynamicIslandShortStopwatch",
            "Countdown" => "DynamicIslandShortCountdown",
            "SystemStats" => "DynamicIslandShortSystem",
            "NetSpeed" => "DynamicIslandShortNetwork",
            "Battery" => "DynamicIslandShortBattery",
            "BluetoothBattery" => "DynamicIslandShortDevices",
            "Storage" => "DynamicIslandShortStorage",
            "Media" => "DynamicIslandShortMedia",
            "Audio" => "DynamicIslandShortAudio",
            "QuickSettings" => "DynamicIslandShortQuickSettings",
            "Focus" => "DynamicIslandShortFocus",
            "KeyboardLayout" => "DynamicIslandShortKeyboard",
            _ => null
        };
        if (shortLabelKey is not null) return UiText.Get(shortLabelKey);
        if (!DockWidgetService.TryGetDescriptor(id, out var descriptor)) return id;
        return UiText.Get(descriptor.LabelKey);
    }

    private void SetExpanded(bool expanded, bool animate = true)
    {
        if (expanded && !HasExpandableContent(CurrentPageId())) return;
        _isExpanded = expanded;
        DetailsPanel.Visibility = expanded ? Visibility.Visible : Visibility.Collapsed;
        if (expanded)
        {
            _details = GetCurrentDetails();
            DetailsContent.Content = _details;
        }
        else
        {
            DetailsContent.Content = null;
            _details = null;
        }

        UpdatePageControls();
        PositionWindow(WidthForCurrentPage(), expanded ? ExpandedHeight : CompactHeight,
            animate && _settings.EffectiveAnimationsEnabled);
    }

    private void ShowDetails(UIElement details, string id)
    {
        if (!string.Equals(id, CurrentPageId(), StringComparison.OrdinalIgnoreCase)) return;
        _details = details;
        DetailsContent.Content = details;
        DetailsPanel.Visibility = Visibility.Visible;
        _isExpanded = true;
        UpdatePageControls();
        PositionWindow(WidthForCurrentPage(), ExpandedHeight, animate: _settings.EffectiveAnimationsEnabled);
    }

    private UIElement? GetCurrentDetails()
    {
        var id = CurrentPageId();
        if (id is null) return null;
        if (_timedWidgets.TryGetValue(id, out var timedWidget))
            return new TimedWidgetPanel { DataContext = timedWidget };
        return _widgetCoordinator.ActiveWidgets.TryGetValue(id, out var widget) ? GetDetails(widget, id) : null;
    }

    private double WidthForCurrentPage()
    {
        var preferred = _isExpanded ? ExpandedSurfaceWidth : CompactSurfaceWidth;
        return DynamicIslandPolicy.ClampWindowWidth(
            preferred, _display.Bounds.Width, MinimumSurfaceWidth, preferred, edgeInset: 28);
    }

    private void PositionWindow(double width, double height, bool animate)
    {
        if (!IsInitialized) return;
        var windowWidth = width + ShoulderWidth * 2;
        IslandSurface.CornerRadius = height > CompactHeight
            ? new CornerRadius(0, 0, 22, 22)
            : new CornerRadius(0, 0, 17, 17);
        var focusRadius = Math.Max(0, IslandSurface.CornerRadius.BottomRight - 2);
        HeaderFocusRing.CornerRadius = new CornerRadius(0, 0, focusRadius, focusRadius);
        UpdateIslandContentClip();
        if (animate && IsVisible)
        {
            AnimateWindowValue(WidthProperty, ActualWidth, windowWidth);
            AnimateWindowValue(HeightProperty, ActualHeight, height);
            AnimateSurfaceWidth(width);
        }
        else
        {
            BeginAnimation(WidthProperty, null);
            BeginAnimation(HeightProperty, null);
            IslandSurface.BeginAnimation(FrameworkElement.WidthProperty, null);
            IslandSurface.Width = width;
            Width = windowWidth;
            Height = height;
        }
        PositionNativeWindow();
    }

    private void PositionNativeWindow()
    {
        var handle = new WindowInteropHelper(this).Handle;
        if (handle == 0) return;
        // WPF converts Left/Top using the window's current monitor DPI, not the destination DPI.
        var scale = _display.Dpi / 96d;
        var left = (int)Math.Round((_display.Bounds.Left + (_display.Bounds.Width - Width) / 2) * scale);
        var top = (int)Math.Round(_display.Bounds.Top * scale);
        const uint noSizeNoZOrderNoActivate = 0x0001 | 0x0004 | 0x0010;
        SetWindowPos(handle, 0, left, top, 0, 0, noSizeNoZOrderNoActivate);
    }

    private void IslandContent_SizeChanged(object sender, SizeChangedEventArgs e) => UpdateIslandContentClip();

    private void UpdateIslandContentClip()
    {
        var width = IslandContent.ActualWidth;
        var height = IslandContent.ActualHeight;
        var radius = IslandSurface.CornerRadius.BottomRight;
        radius = Math.Min(radius, Math.Min(width / 2, height));
        if (width <= 0 || height <= 0 || radius <= 0)
        {
            IslandContent.Clip = null;
            return;
        }

        var clip = new StreamGeometry();
        using (var context = clip.Open())
        {
            context.BeginFigure(new Point(0, 0), isFilled: true, isClosed: true);
            context.LineTo(new Point(width, 0), isStroked: true, isSmoothJoin: false);
            context.LineTo(new Point(width, height - radius), isStroked: true, isSmoothJoin: false);
            context.ArcTo(new Point(width - radius, height), new Size(radius, radius), 0,
                isLargeArc: false, SweepDirection.Clockwise, isStroked: true, isSmoothJoin: false);
            context.LineTo(new Point(radius, height), isStroked: true, isSmoothJoin: false);
            context.ArcTo(new Point(0, height - radius), new Size(radius, radius), 0,
                isLargeArc: false, SweepDirection.Clockwise, isStroked: true, isSmoothJoin: false);
            context.LineTo(new Point(0, 0), isStroked: true, isSmoothJoin: false);
        }
        clip.Freeze();
        IslandContent.Clip = clip;
    }

    private void AnimateSurfaceWidth(double width)
    {
        var animation = new DoubleAnimation(IslandSurface.ActualWidth, width, ResizeDuration)
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            FillBehavior = FillBehavior.HoldEnd
        };
        animation.Completed += (_, _) =>
        {
            IslandSurface.Width = width;
            IslandSurface.BeginAnimation(FrameworkElement.WidthProperty, null);
        };
        IslandSurface.BeginAnimation(FrameworkElement.WidthProperty, animation, HandoffBehavior.SnapshotAndReplace);
    }

    private void AnimateWindowValue(DependencyProperty property, double current, double target)
    {
        var animation = new DoubleAnimation(current, target, ResizeDuration)
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            FillBehavior = FillBehavior.HoldEnd
        };
        animation.Completed += (_, _) =>
        {
            SetValue(property, target);
            BeginAnimation(property, null);
        };
        BeginAnimation(property, animation, HandoffBehavior.SnapshotAndReplace);
    }

    private string? CurrentPageId() => _pageIds.Count == 0 ? null : _pageIds[CurrentPageIndex()];

    private int CurrentPageIndex() => _pageIds.Count == 0 ? 0 : _currentPageIndex;

    private void PageViewport_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        Pages.BeginAnimation(DynamicIslandPagePanel.HorizontalOffsetProperty, null);
        Pages.HorizontalOffset = -_currentPageIndex * Math.Max(1, e.NewSize.Width);
        foreach (var page in Pages.Children.OfType<Grid>()) page.Width = e.NewSize.Width;
    }

    private void PageViewport_MouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (e.Handled || _isDragging || IsInteractiveSource(e.OriginalSource as DependencyObject)) return;
        SetExpanded(!_isExpanded);
        e.Handled = true;
    }

    private void SetCurrentPageIndex(int index, bool animate)
    {
        if (_pageIds.Count == 0) return;
        var targetIndex = Math.Clamp(index, 0, _pageIds.Count - 1);
        _currentPageIndex = targetIndex;
        var targetOffset = -targetIndex * Math.Max(1, PageViewport.ActualWidth);
        if (animate && _settings.EffectiveAnimationsEnabled)
        {
            var animation = new DoubleAnimation(Pages.HorizontalOffset, targetOffset, SlideDuration)
            {
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
                FillBehavior = FillBehavior.HoldEnd
            };
            animation.Completed += (_, _) =>
            {
                Pages.HorizontalOffset = targetOffset;
                Pages.BeginAnimation(DynamicIslandPagePanel.HorizontalOffsetProperty, null);
                if (!_resourcesDisposed)
                    PositionWindow(WidthForCurrentPage(), _isExpanded ? ExpandedHeight : CompactHeight, animate: true);
            };
            Pages.BeginAnimation(DynamicIslandPagePanel.HorizontalOffsetProperty, animation);
        }
        else
        {
            Pages.BeginAnimation(DynamicIslandPagePanel.HorizontalOffsetProperty, null);
            Pages.HorizontalOffset = targetOffset;
        }

        if (_isExpanded && HasExpandableContent(CurrentPageId()))
        {
            _details = GetCurrentDetails();
            DetailsContent.Content = _details;
            DetailsPanel.Visibility = Visibility.Visible;
        }
        else
        {
            _details = null;
            DetailsContent.Content = null;
            DetailsPanel.Visibility = Visibility.Collapsed;
            _isExpanded = false;
        }
        UpdatePageControls();
        if (!animate || !_settings.EffectiveAnimationsEnabled)
            PositionWindow(WidthForCurrentPage(), _isExpanded ? ExpandedHeight : CompactHeight, animate: false);
    }

    private void MovePage(int delta)
    {
        if (_pageIds.Count <= 1) return;
        var targetIndex = Math.Clamp(CurrentPageIndex() + delta, 0, _pageIds.Count - 1);
        if (targetIndex == CurrentPageIndex()) return;
        SetCurrentPageIndex(targetIndex, animate: true);
    }

    private void Header_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e) => BeginPointerGesture(e, allowButtons: false);
    private void Details_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e) => BeginPointerGesture(e, allowButtons: true);

    private void Header_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key is not (Key.Enter or Key.Space)
            || !ReferenceEquals(Keyboard.FocusedElement, Header)) return;
        SetExpanded(!_isExpanded);
        e.Handled = true;
    }

    private void Header_GotKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e) => HeaderFocusRing.Opacity = 1;

    private void Header_LostKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e) => HeaderFocusRing.Opacity = 0;

    private void BeginPointerGesture(MouseButtonEventArgs e, bool allowButtons)
    {
        _isDragging = false;
        _pointerGestureActive = false;
        var source = e.OriginalSource as DependencyObject;
        if (e.ChangedButton != MouseButton.Left || _pageIds.Count <= 1
            || (allowButtons ? IsDragControl(source) : IsInteractiveSource(source))) return;
        _pointerGestureActive = true;
        _pointerOrigin = e.GetPosition(PageViewport);
        _dragStartIndex = _currentPageIndex;
        _dragOffset = 0;
    }

    private void Header_PreviewMouseMove(object sender, MouseEventArgs e) => UpdatePointerGesture(e);
    private void Details_PreviewMouseMove(object sender, MouseEventArgs e) => UpdatePointerGesture(e);

    private void UpdatePointerGesture(MouseEventArgs e)
    {
        if (!_pointerGestureActive || e.LeftButton != MouseButtonState.Pressed || _pageIds.Count <= 1) return;
        var position = e.GetPosition(PageViewport);
        var dx = position.X - _pointerOrigin.X;
        var dy = position.Y - _pointerOrigin.Y;
        if (!_isDragging)
        {
            if (Math.Abs(dx) < SystemParameters.MinimumHorizontalDragDistance
                || Math.Abs(dx) < Math.Abs(dy)) return;
            _isDragging = true;
            Pages.BeginAnimation(DynamicIslandPagePanel.HorizontalOffsetProperty, null);
            Pages.HorizontalOffset = -_dragStartIndex * Math.Max(1, PageViewport.ActualWidth);
            Header.CaptureMouse();
        }

        var pageWidth = Math.Max(1, PageViewport.ActualWidth);
        _dragOffset = dx;
        var baseOffset = -_dragStartIndex * pageWidth;
        var targetIndex = _dragStartIndex + (dx < 0 ? 1 : -1);
        var resisted = targetIndex < 0 || targetIndex >= _pageIds.Count ? dx * 0.22 : dx;
        Pages.HorizontalOffset = baseOffset + Math.Clamp(resisted, -pageWidth, pageWidth);
        e.Handled = true;
    }

    private void Header_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e) => FinishPointerGesture(e);
    private void Details_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e) => FinishPointerGesture(e);

    private void Header_LostMouseCapture(object sender, MouseEventArgs e)
    {
        _pointerGestureActive = false;
        if (!_isDragging) return;
        _isDragging = false;
        SetCurrentPageIndex(_dragStartIndex, animate: true);
    }

    private void FinishPointerGesture(MouseButtonEventArgs e)
    {
        _pointerGestureActive = false;
        if (!_isDragging) return;
        _isDragging = false;
        if (Header.IsMouseCaptured) Header.ReleaseMouseCapture();
        var threshold = Math.Max(36, PageViewport.ActualWidth * 0.18);
        _dragOffset = e.GetPosition(PageViewport).X - _pointerOrigin.X;
        var delta = Math.Abs(_dragOffset) >= threshold ? Math.Sign(_dragOffset) : 0;
        if (delta != 0)
            SetCurrentPageIndex(_dragStartIndex + (delta < 0 ? 1 : -1), animate: true);
        else
            SetCurrentPageIndex(_dragStartIndex, animate: true);
        e.Handled = true;
    }

    private void Window_PreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Escape && _isExpanded)
        {
            SetExpanded(false);
            e.Handled = true;
        }
        else if (e.Key is Key.Left or Key.Right && !IsInteractiveSource(Keyboard.FocusedElement as DependencyObject))
        {
            MovePage(e.Key == Key.Right ? 1 : -1);
            e.Handled = true;
        }
    }

    private void InputManager_PreProcessInput(object sender, PreProcessInputEventArgs e)
    {
        if (e.StagingItem.Input is not KeyEventArgs { RoutedEvent: var routed, Key: var key } input
            || routed != Keyboard.KeyDownEvent || !PointerIsOverIsland()
            || IsInteractiveSource(Keyboard.FocusedElement as DependencyObject)) return;
        if (key == Key.Escape && _isExpanded)
        {
            SetExpanded(false);
            input.Handled = true;
        }
        else if (key is Key.Left or Key.Right)
        {
            MovePage(key == Key.Right ? 1 : -1);
            input.Handled = true;
        }
    }

    private void Window_PreviewMouseWheel(object sender, MouseWheelEventArgs e)
    {
        if (!IsWithin(e.OriginalSource as DependencyObject, DetailsScrollViewer)) e.Handled = true;
    }

    private static bool IsInteractiveSource(DependencyObject? source)
    {
        var current = source;
        while (current is not null)
        {
            if (current is ButtonBase or Slider or TextBoxBase or Selector or ScrollBar or Thumb) return true;
            current = GetParent(current);
        }
        return false;
    }

    private static bool IsDragControl(DependencyObject? source)
    {
        var current = source;
        while (current is not null)
        {
            if (current is Slider or TextBoxBase or Selector or ScrollBar or Thumb) return true;
            current = GetParent(current);
        }
        return false;
    }

    private bool PointerIsOverIsland()
    {
        var current = Mouse.DirectlyOver as DependencyObject;
        while (current is not null)
        {
            if (ReferenceEquals(current, this)) return true;
            current = GetParent(current);
        }
        return false;
    }

    private static bool IsWithin(DependencyObject? source, DependencyObject target)
    {
        var current = source;
        while (current is not null)
        {
            if (ReferenceEquals(current, target)) return true;
            current = GetParent(current);
        }
        return false;
    }

    private static DependencyObject? GetParent(DependencyObject current) => current is Visual or System.Windows.Media.Media3D.Visual3D
        ? VisualTreeHelper.GetParent(current)
        : current is FrameworkContentElement content ? content.Parent : null;

    private void UpdateControlLabels()
    {
        Title = UiText.Get("DynamicIslandTitle");
        UpdatePageControls();
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e) => UpdateControlLabels();

    private void VisibilityTimer_Tick(object? sender, EventArgs e) => RefreshVisibility();

    private void RevealTimer_Tick(object? sender, EventArgs e)
    {
        if (_resourcesDisposed || IsVisible || !_settings.DynamicIslandAutoHide)
        {
            _revealTimer.Stop();
            return;
        }

        if (IsPointerAtRevealStrip()) RefreshVisibility();
    }

    private void RefreshVisibility()
    {
        if (_resourcesDisposed) return;
        var foregroundAvailable = NativeWindowTracker.TryGetForegroundApplicationWindow(out var foreground);
        var foregroundOnDisplay = foregroundAvailable
            && foreground != 0
            && SecondaryDockWindow.IsWindowOnMonitor(foreground, _display.Handle);
        var fullscreen = _settings.DynamicIslandHideInFullscreen
            && TaskbarReplacementSession.IsAnyFullScreenOnMonitor(_display.Handle);
        var shouldShow = DynamicIslandPolicy.ShouldCreateWindow(
                _settings.DynamicIslandEnabled, _settings.FocusModeEnabled)
            && DynamicIslandPolicy.ShouldShow(
                _settings.DynamicIslandVisibility,
                desktopVisible: !foregroundOnDisplay,
                hideInFullscreen: _settings.DynamicIslandHideInFullscreen,
                fullscreen);

        if (shouldShow && _settings.DynamicIslandAutoHide)
        {
            if (IsVisible)
            {
                _revealTimer.Stop();
                shouldShow = IsMouseOver;
            }
            else
            {
                shouldShow = IsPointerAtRevealStrip();
                if (shouldShow) _revealTimer.Stop();
                else
                {
                    _hiddenByAutoHide = true;
                    if (!_revealTimer.IsEnabled) _revealTimer.Start();
                }
            }
        }
        else
        {
            _revealTimer.Stop();
            if (!shouldShow) _hiddenByAutoHide = false;
        }

        if (shouldShow && !IsVisible)
        {
            if (_hiddenByAutoHide && _settings.DynamicIslandAutoHide) ShowWithRevealAnimation();
            else Show();
            _hiddenByAutoHide = false;
        }
        else if (!shouldShow && IsVisible)
        {
            _hiddenByAutoHide = _settings.DynamicIslandAutoHide;
            Hide();
        }
    }

    private void ShowWithRevealAnimation()
    {
        if (!_settings.EffectiveAnimationsEnabled || !SystemParameters.ClientAreaAnimation)
        {
            IslandRoot.RenderTransform = Transform.Identity;
            IslandRoot.Opacity = 1;
            Show();
            return;
        }

        var distance = Math.Max(CompactHeight, Height);
        var transform = new TranslateTransform(0, -distance);
        IslandRoot.RenderTransform = transform;
        IslandRoot.Opacity = 0;
        Show();

        var slide = new DoubleAnimation(-distance, 0, TimeSpan.FromMilliseconds(190))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            FillBehavior = FillBehavior.HoldEnd
        };
        slide.Completed += (_, _) =>
        {
            transform.Y = 0;
            transform.BeginAnimation(TranslateTransform.YProperty, null);
        };
        transform.BeginAnimation(TranslateTransform.YProperty, slide, HandoffBehavior.SnapshotAndReplace);

        var fade = new DoubleAnimation(0, 1, TimeSpan.FromMilliseconds(135))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            FillBehavior = FillBehavior.HoldEnd
        };
        fade.Completed += (_, _) =>
        {
            IslandRoot.Opacity = 1;
            IslandRoot.BeginAnimation(UIElement.OpacityProperty, null);
        };
        IslandRoot.BeginAnimation(UIElement.OpacityProperty, fade, HandoffBehavior.SnapshotAndReplace);
    }

    private bool IsPointerAtRevealStrip()
    {
        if (!GetCursorPos(out var cursor)) return false;
        var scale = _display.Dpi / 96d;
        var left = (_display.Bounds.Left + (_display.Bounds.Width - Width) / 2) * scale;
        var top = _display.Bounds.Top * scale;
        return cursor.X >= left && cursor.X <= left + Width * scale
            && cursor.Y >= top && cursor.Y <= top + RevealStripHeight * scale;
    }

    private void Window_Closed(object? sender, EventArgs e) => ReleaseResources();

    private void ReleaseResources()
    {
        if (_resourcesDisposed) return;
        _resourcesDisposed = true;
        _visibilityTimer.Stop();
        _visibilityTimer.Tick -= VisibilityTimer_Tick;
        _revealTimer.Stop();
        _revealTimer.Tick -= RevealTimer_Tick;
        InputManager.Current.PreProcessInput -= InputManager_PreProcessInput;
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        _widgetCoordinator.AvailabilityChanged -= SyncPages;
        _widgetCoordinator.WidgetsPlaced -= SyncPages;
        _widgetCoordinator.WidgetRemoveRequested -= _removeWidget;
        _widgetCoordinator.Dispose();
    }

    protected override void OnSourceInitialized(EventArgs e)
    {
        base.OnSourceInitialized(e);
        var handle = new WindowInteropHelper(this).Handle;
        var style = GetWindowLongPtr(handle, -20).ToInt64();
        SetWindowLongPtr(handle, -20, new nint((style | WsExToolWindow | WsExNoActivate) & ~WsExAppWindow));
        HwndSource.FromHwnd(handle)?.AddHook(WindowProc);
        SizeChanged += (_, _) => PositionNativeWindow();
        PositionNativeWindow();
    }

    private static nint WindowProc(nint hwnd, int message, nint wParam, nint lParam, ref bool handled)
    {
        if (message != WmMouseActivate) return 0;
        handled = true;
        return new nint(MaNoActivate);
    }

    private static nint GetWindowLongPtr(nint window, int index) => IntPtr.Size == 8
        ? GetWindowLongPtr64(window, index)
        : new nint(GetWindowLong32(window, index));

    private static void SetWindowLongPtr(nint window, int index, nint value)
    {
        if (IntPtr.Size == 8) _ = SetWindowLongPtr64(window, index, value);
        else _ = SetWindowLong32(window, index, value.ToInt32());
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out NativePoint point);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint hwnd, nint after, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr64(nint window, int index);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
    private static extern int GetWindowLong32(nint window, int index);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern nint SetWindowLongPtr64(nint window, int index, nint value);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongW")]
    private static extern int SetWindowLong32(nint window, int index, int value);
}
