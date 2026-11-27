using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Owns the dock widgets: builds them into the strip, applies order and visibility from settings,
/// supports drag-and-drop reordering with fluid animations directly on the dock, and drives a shared 1 s tick.
/// </summary>
public sealed class WidgetCoordinator : IDisposable
{
    public sealed record WidgetItem(string Id, string LabelKey, DockWidget Widget, Func<DockSettings, bool> IsEnabled);

    /// <summary>Dock slots come from the single widget registry in its explicit DockOrder.</summary>
    public static readonly string[] DefaultWidgetOrder =
        DockWidgetService.LiveDescriptors.Select(descriptor => descriptor.Id).ToArray();

    public static string? GetLabelKey(string id)
    {
        var descriptor = DockWidgetService.LiveDescriptors
            .FirstOrDefault(item => string.Equals(item.Id, id, StringComparison.OrdinalIgnoreCase));
        return descriptor?.LabelKey;
    }

    public static bool HandlesWidget(string id) =>
        DefaultWidgetOrder.Contains(id, StringComparer.OrdinalIgnoreCase);

    /// <summary>
    /// Creates the same real widget instance the dock uses. Settings uses this for live catalog previews so the
    /// cards show the actual widget visuals and data instead of hand-authored static mock templates.
    /// </summary>
    public static DockWidget CreateWidget(string id)
    {
        var descriptor = DockWidgetService.LiveDescriptors
            .FirstOrDefault(item => string.Equals(item.Id, id, StringComparison.OrdinalIgnoreCase));
        if (descriptor?.Factory is { } factory) return factory();
        throw new ArgumentException($"Unknown widget id '{id}'.", nameof(id));
    }

    private sealed class WidgetSlot(string id, string labelKey, Func<DockWidget> factory, Func<DockSettings, bool> isEnabled)
    {
        public string Id { get; } = id;
        public string LabelKey { get; } = labelKey;
        public Func<DockWidget> Factory { get; } = factory;
        public Func<DockSettings, bool> IsEnabled { get; } = isEnabled;
        public DockWidget? Widget { get; set; }
    }

    public sealed record WidgetDragPayload(string WidgetId);

    private readonly Panel _strip;
    private readonly Action<string> _reportError;
    private readonly List<WidgetSlot> _widgets;
    private readonly DispatcherTimer _timer;
    private readonly WidgetHoverCoordinator _hoverCoordinator;
    private readonly HashSet<DockWidget> _displacedWidgets = [];
    private Point _dragStartPoint;
    private DockWidget? _draggedWidget;
    private bool _disposed;
    private bool _batchingApply;
    private bool _availabilityChangePending;

    public Action<IReadOnlyList<string>>? OrderChanged { get; set; }
    public Action<DockSettings>? WidgetSettingsUpdated { get; set; }
    public event Action? WidgetsPlaced;
    public event Action<string>? WidgetRemoveRequested;
    /// <summary>Raised when a widget drag ends, so the host can clean up any live-reorder state.</summary>
    public event Action? DragEnded;

    /// <summary>Lets the host wire its context-menu open/close handlers so auto-hide stays locked while the menu is up.</summary>
    public Action<ContextMenu>? ConfigureWidgetMenu { get; set; }
    public Func<string, ContextMenu>? CreateWidgetContextMenu { get; set; }
    public Func<string, IEnumerable<Control>>? CreateWidgetAdditionalMenuItems { get; set; }
    public Func<DockSettings, string, bool>? IsWidgetEnabled { get; set; }
    public bool AllowWidgetReorder { get; set; } = true;
    public bool EnableWidgetHover { get; set; } = true;
    public IReadOnlyList<string> CurrentOrder => _widgets.Select(w => w.Id).ToArray();

    /// <summary>When true, the host window owns widget placement so all dock item kinds share one strip.</summary>
    public bool ExternalLayout { get; set; }

    /// <summary>Create a visual preview using the real widget control without activating its background polling.</summary>
    public static DockWidget? CreatePreviewWidget(string id) =>
        HandlesWidget(id) ? CreateWidget(id) : null;

    public IReadOnlyDictionary<string, DockWidget> ActiveWidgets => _widgets
        .Where(slot => slot.Widget is not null)
        .ToDictionary(slot => slot.Id, slot => slot.Widget!);

    public bool HasActiveWidgets { get; private set; }

    /// <summary>Raised when a widget appears or disappears and the dock must refit its layout.</summary>
    public event Action? AvailabilityChanged;

    public bool IsHoverActive => _hoverCoordinator.IsHoverActive;
    public event Action<bool>? HoverActiveChanged
    {
        add => _hoverCoordinator.HoverActiveChanged += value;
        remove => _hoverCoordinator.HoverActiveChanged -= value;
    }
    public void CollapseHover() => _hoverCoordinator.CollapseActive();
    public void SuppressHover(TimeSpan duration) => _hoverCoordinator.Suppress(duration);

    private static bool IsCatalogEnabled(DockSettings settings, string id) =>
        settings.Widgets?.Contains(id, StringComparer.OrdinalIgnoreCase) == true;

    public WidgetCoordinator(
        Panel strip,
        Dispatcher dispatcher,
        Action<string> reportError,
        bool lazy = false,
        Func<DockWidget, bool>? canExpandInline = null,
        Action<bool>? onHoverLayoutChanged = null,
        Func<string, DockWidget>? createWidget = null)
    {
        _strip = strip;
        _reportError = reportError;
        _hoverCoordinator = new WidgetHoverCoordinator(
            dispatcher,
            canExpandInline ?? (_ => false),
            onHoverLayoutChanged ?? (_ => { }));
        _widgets = DockWidgetService.LiveDescriptors
            .Select(descriptor => new WidgetSlot(
                descriptor.Id,
                descriptor.LabelKey,
                () => (createWidget ?? CreateWidget)(descriptor.Id),
                s => (IsCatalogEnabled(s, descriptor.Id)
                    || descriptor.AliasIds?.Any(alias => IsCatalogEnabled(s, alias)) == true)
                    && (IsWidgetEnabled?.Invoke(s, descriptor.Id) ?? true)))
            .ToList();

        if (!lazy)
        {
            foreach (var slot in _widgets)
            {
                EnsureWidget(slot);
            }
        }

        PlaceWidgets();

        _timer = new DispatcherTimer(DispatcherPriority.Background, dispatcher) { Interval = TimeSpan.FromSeconds(1) };
        _timer.Tick += Timer_Tick;
    }

    public string? GetWidgetId(DockWidget widget) =>
        (widget.Tag as string) ?? _widgets.FirstOrDefault(s => ReferenceEquals(s.Widget, widget))?.Id;

    private DockWidget EnsureWidget(WidgetSlot slot)
    {
        if (slot.Widget is { } existing) return existing;
        var widget = slot.Factory();
        widget.Tag = slot.Id;
        widget.ReportError = _reportError;
        widget.UpdateSettings = updated =>
        {
            if (WidgetSettingsUpdated is not null)
            {
                WidgetSettingsUpdated(updated);
            }
            else
            {
                widget.ApplySettings(updated);
            }
        };
        widget.AllowDrop = true;
        widget.PreviewMouseLeftButtonDown += Widget_PreviewMouseLeftButtonDown;
        widget.PreviewMouseLeftButtonUp += Widget_PreviewMouseLeftButtonUp;
        widget.PreviewMouseMove += Widget_PreviewMouseMove;
        widget.DragOver += Widget_DragOver;
        widget.DragLeave += Widget_DragLeave;
        widget.Drop += Widget_Drop;
        widget.AvailabilityChanged += Widget_AvailabilityChanged;
        widget.ContentWidthChanged += Widget_AvailabilityChanged;

        var customItems = widget.BuildContextMenuItems()?.ToList() ?? [];
        if (CreateWidgetAdditionalMenuItems?.Invoke(slot.Id) is { } additionalItems)
            customItems.AddRange(additionalItems);
        widget.ContextMenu = customItems.Count > 0
            ? CreateWidgetMenuWithCustomItems(slot.Id, customItems)
            : CreateWidgetContextMenu?.Invoke(slot.Id) ?? CreateDefaultWidgetMenu(slot.Id);

        if (EnableWidgetHover) _hoverCoordinator.Attach(widget);
        slot.Widget = widget;
        return widget;
    }

    private ContextMenu CreateWidgetMenuWithCustomItems(string widgetId, IReadOnlyList<Control> customItems)
    {
        var menu = new ContextMenu();
        foreach (var item in customItems)
        {
            menu.Items.Add(item);
        }

        menu.Items.Add(new Separator());
        menu.Items.Add(CreateRemoveWidgetMenuItem(widgetId, menu));
        ConfigureWidgetMenu?.Invoke(menu);
        return menu;
    }

    private ContextMenu CreateDefaultWidgetMenu(string widgetId)
    {
        var menu = new ContextMenu();
        menu.Items.Add(CreateRemoveWidgetMenuItem(widgetId, menu));
        ConfigureWidgetMenu?.Invoke(menu);
        return menu;
    }

    private MenuItem CreateRemoveWidgetMenuItem(string widgetId, ContextMenu menu)
    {
        var removeItem = new MenuItem { Header = UiText.Get("RemoveWidget") };
        if (Application.Current?.TryFindResource("IconTrash") is Geometry icon)
        {
            removeItem.Icon = icon;
        }
        removeItem.Click += (_, _) => WidgetRemoveRequested?.Invoke(widgetId);
        menu.Opened += (_, _) => removeItem.Header = UiText.Get("RemoveWidget");
        return removeItem;
    }

    public void Apply(DockSettings settings)
    {
        if (_disposed) return;

        _batchingApply = true;
        try
        {
            if (settings.WidgetOrder is { Count: > 0 } order)
            {
                ApplyOrder(order);
            }

            var anyActive = false;
            var stripNeedsSync = false;
            foreach (var slot in _widgets)
            {
                if (!settings.FocusModeEnabled && slot.IsEnabled(settings))
                {
                    var wasCreated = slot.Widget is null;
                    var widget = EnsureWidget(slot);
                    if (wasCreated) stripNeedsSync = true;
                    widget.ApplySettings(settings);
                    widget.Activate();
                    anyActive = true;
                }
                else if (slot.Widget is { } widget)
                {
                    widget.ApplySettings(settings);
                    widget.Deactivate();
                }
            }

            if (stripNeedsSync)
            {
                PlaceWidgets();
            }

            HasActiveWidgets = anyActive;

            if (anyActive)
            {
                Timer_Tick(null, EventArgs.Empty);
                _timer.Start();
            }
            else
            {
                _timer.Stop();
            }
        }
        finally
        {
            _batchingApply = false;
            if (_availabilityChangePending)
            {
                _availabilityChangePending = false;
                AvailabilityChanged?.Invoke();
            }
        }
    }

    public void ApplyOrder(IReadOnlyList<string> order)
    {
        var orderMap = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        for (var i = 0; i < order.Count; i++)
        {
            orderMap[order[i]] = i;
        }

        // List<T>.Sort is not stable; preserve the relative order of unspecified widgets.
        var originalPositions = _widgets.Select((slot, index) => (slot.Id, index))
            .ToDictionary(item => item.Id, item => item.index, StringComparer.OrdinalIgnoreCase);
        _widgets.Sort((a, b) =>
        {
            var indexA = orderMap.TryGetValue(a.Id, out var ia) ? ia : int.MaxValue;
            var indexB = orderMap.TryGetValue(b.Id, out var ib) ? ib : int.MaxValue;
            var comparison = indexA.CompareTo(indexB);
            return comparison != 0 ? comparison : originalPositions[a.Id].CompareTo(originalPositions[b.Id]);
        });

        PlaceWidgets();
    }

    public void ReorderWidgets(int oldIndex, int newIndex)
    {
        if (oldIndex < 0 || oldIndex >= _widgets.Count || newIndex < 0 || newIndex >= _widgets.Count || oldIndex == newIndex)
        {
            return;
        }

        var item = _widgets[oldIndex];
        _widgets.RemoveAt(oldIndex);
        _widgets.Insert(newIndex, item);

        PlaceWidgets();

        var newOrder = _widgets.Select(w => w.Id).ToArray();
        OrderChanged?.Invoke(newOrder);
    }

    internal static (ScaleTransform Scale, TranslateTransform Translate) GetWidgetTransforms(DockWidget widget)
    {
        if (widget.RenderTransform is TransformGroup { IsFrozen: false, IsSealed: false } group)
        {
            var s = group.Children.OfType<ScaleTransform>().FirstOrDefault();
            var t = group.Children.OfType<TranslateTransform>().FirstOrDefault();
            if (s is { IsFrozen: false, IsSealed: false } && t is { IsFrozen: false, IsSealed: false })
            {
                return (s, t);
            }
        }

        var scale = new ScaleTransform();
        var translate = new TranslateTransform();
        widget.RenderTransform = new TransformGroup { Children = { scale, translate } };
        widget.RenderTransformOrigin = new Point(0.5, 0.5);
        return (scale, translate);
    }

    private void AnimateWidgetDrag(DockWidget widget, string widgetId)
    {
        try
        {
            DragGhost.DoDragDrop(widget, new WidgetDragPayload(widgetId), DragDropEffects.Move);
        }
        finally
        {
            DragEnded?.Invoke();
            ResetWidgetDisplacements();
        }
    }

    private void UpdateWidgetDisplacement(DockWidget targetWidget, DragEventArgs e)
    {
        var pos = e.GetPosition(targetWidget);
        var isRightHalf = pos.X > targetWidget.ActualWidth / 2;
        var shift = isRightHalf ? -10.0 : 10.0;

        var (_, translate) = GetWidgetTransforms(targetWidget);
        var anim = new DoubleAnimation(shift, TimeSpan.FromMilliseconds(130))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        };
        translate.BeginAnimation(TranslateTransform.XProperty, anim);
        _displacedWidgets.Add(targetWidget);
    }

    private void ResetWidgetDisplacement(DockWidget widget)
    {
        if (!_displacedWidgets.Remove(widget)) return;
        var (_, translate) = GetWidgetTransforms(widget);
        var anim = new DoubleAnimation(0, TimeSpan.FromMilliseconds(130))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        };
        translate.BeginAnimation(TranslateTransform.XProperty, anim);
    }

    private void ResetWidgetDisplacements()
    {
        foreach (var widget in _displacedWidgets)
        {
            var (_, translate) = GetWidgetTransforms(widget);
            translate.BeginAnimation(TranslateTransform.XProperty, null);
            translate.X = 0;
        }
        _displacedWidgets.Clear();
    }

    private static void AnimateWidgetDropSettle(DockWidget widget)
    {
        var (scale, _) = GetWidgetTransforms(widget);
        var settleScale = new DoubleAnimationUsingKeyFrames
        {
            Duration = TimeSpan.FromMilliseconds(200),
            KeyFrames =
            {
                new EasingDoubleKeyFrame(1.06, KeyTime.FromTimeSpan(TimeSpan.Zero)),
                new EasingDoubleKeyFrame(0.98, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(110)), new CubicEase { EasingMode = EasingMode.EaseOut }),
                new EasingDoubleKeyFrame(1.0, KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(200)), new CubicEase { EasingMode = EasingMode.EaseOut })
            }
        };

        scale.BeginAnimation(ScaleTransform.ScaleXProperty, settleScale);
        scale.BeginAnimation(ScaleTransform.ScaleYProperty, settleScale);
    }

    private void Widget_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (!AllowWidgetReorder) return;
        if (e.OriginalSource is DependencyObject dep && IsInteractiveControl(dep))
        {
            _draggedWidget = null;
            return;
        }

        if (sender is DockWidget widget)
        {
            _dragStartPoint = e.GetPosition(_strip);
            _draggedWidget = widget;
        }
    }

    internal static bool IsInteractiveControl(DependencyObject dep)
    {
        var current = dep;
        while (current is not null)
        {
            if (current is Slider or Thumb or TextBoxBase)
            {
                return true;
            }
            if (current is Visual or System.Windows.Media.Media3D.Visual3D)
            {
                current = VisualTreeHelper.GetParent(current);
            }
            else if (current is FrameworkContentElement fce)
            {
                current = fce.Parent;
            }
            else
            {
                break;
            }
        }
        return false;
    }

    private void Widget_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        _draggedWidget = null;
    }

    private void Widget_PreviewMouseMove(object sender, MouseEventArgs e)
    {
        if (!AllowWidgetReorder || e.LeftButton != MouseButtonState.Pressed
            || _draggedWidget is null || sender is not DockWidget widget)
        {
            return;
        }

        var currentPoint = e.GetPosition(_strip);
        var diff = _dragStartPoint - currentPoint;
        if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
        {
            var targetWidget = _draggedWidget;
            _draggedWidget = null;
            var widgetId = _widgets.FirstOrDefault(w => ReferenceEquals(w.Widget, targetWidget))?.Id;
            if (widgetId is null) return;

            if (Mouse.Captured is UIElement captured)
            {
                captured.ReleaseMouseCapture();
            }
            e.Handled = true;
            AnimateWidgetDrag(targetWidget, widgetId);
        }
    }

    private void Widget_DragOver(object sender, DragEventArgs e)
    {
        if (ExternalLayout)
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = false;
            return;
        }

        if (e.Data.GetDataPresent(typeof(WidgetDragPayload)) && sender is DockWidget targetWidget)
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            UpdateWidgetDisplacement(targetWidget, e);
        }
        else if (e.Data.GetDataPresent(typeof(StartButtonDragData)) || e.Data.GetDataPresent("OpenTaskbar.StartButtonDragData"))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = false;
        }
        else
        {
            e.Effects = DragDropEffects.None;
            e.Handled = true;
        }
    }

    private void Widget_DragLeave(object sender, DragEventArgs e)
    {
        if (sender is DockWidget widget)
        {
            ResetWidgetDisplacement(widget);
        }
    }

    private void Widget_Drop(object sender, DragEventArgs e)
    {
        if (ExternalLayout)
        {
            e.Handled = false;
            return;
        }

        if (e.Data.GetDataPresent(typeof(StartButtonDragData)) || e.Data.GetDataPresent("OpenTaskbar.StartButtonDragData"))
        {
            e.Handled = false;
            return;
        }

        if (e.Data.GetData(typeof(WidgetDragPayload)) is WidgetDragPayload payload
            && sender is DockWidget targetWidget)
        {
            var oldIndex = _widgets.FindIndex(w => string.Equals(w.Id, payload.WidgetId, StringComparison.OrdinalIgnoreCase));
            var newIndex = _widgets.FindIndex(w => ReferenceEquals(w.Widget, targetWidget));

            if (oldIndex >= 0 && newIndex >= 0 && oldIndex != newIndex)
            {
                ResetWidgetDisplacements();
                ReorderWidgets(oldIndex, newIndex);
                AnimateWidgetDropSettle(targetWidget);
            }
        }
        ResetWidgetDisplacements();
        e.Handled = true;
    }

    /// <summary>Puts each widget in the dock strip or the overflow panel, keeping the user's order in both.</summary>
    /// <summary>Puts each active widget in the dock strip in the user's configured order.</summary>
    private void PlaceWidgets()
    {
        if (ExternalLayout)
        {
            WidgetsPlaced?.Invoke();
            return;
        }

        _strip.Children.Clear();
        foreach (var widget in _widgets.Where(item => item.Widget is not null).Select(item => item.Widget!))
        {
            _strip.Children.Add(widget);
        }
        WidgetsPlaced?.Invoke();
    }

    private void Widget_AvailabilityChanged(object? sender, EventArgs e)
    {
        if (_batchingApply)
        {
            _availabilityChangePending = true;
            return;
        }

        AvailabilityChanged?.Invoke();
    }

    private void Timer_Tick(object? sender, EventArgs e)
    {
        var now = DateTime.Now;
        foreach (var item in _widgets) item.Widget?.Tick(now);
    }

    /// <summary>
    /// Width of the app area that counts toward widget-density overflow. Apps may scroll instead of
    /// shrinking widgets, but only down to a reserved floor (roughly two dock cells). Below that floor
    /// the dock is genuinely narrow, so the remaining overflow must be recovered from widget density.
    /// </summary>
    public static double EffectiveAppsWidth(double appsContentWidth, double maxAppsWidth, double appScrollFloor)
        => Math.Min(Math.Max(0, appsContentWidth),
            Math.Max(Math.Max(0, maxAppsWidth), Math.Max(0, appScrollFloor)));

    /// <summary>
    /// Compact Auto widgets only when the content would exceed the available dock width.
    /// Explicit Compact and Full choices always take precedence. The full-width estimate
    /// accounts for widgets already compacted on a previous layout pass.
    /// </summary>
    public bool UpdateAdaptiveDensity(
        double maxTotalWidth,
        double appsContentWidth,
        DockSettings settings,
        double hysteresisMargin = 32d,
        double widgetScale = 1.0)
    {
        if (_disposed) return false;

        var candidates = new List<(DockWidget Widget, double Saving)>();
        var projectedFullWidth = appsContentWidth;
        var anyAutoCompact = false;
        var changed = false;

        foreach (var slot in _widgets.Where(slot => slot.Widget is { Visibility: Visibility.Visible }))
        {
            var widget = slot.Widget!;
            var mode = ResolveWidgetDensity(settings, slot.Id);

            if (!widget.SupportsCompactMode) continue;

            var saving = Math.Max(0, widget.FullWidthEstimate - widget.CompactWidthEstimate)
                * Math.Max(0, widgetScale);

            if (mode == WidgetDensityMode.Compact)
            {
                if (!widget.IsCompact) projectedFullWidth -= saving;
                continue;
            }

            if (widget.IsCompact) projectedFullWidth += saving;
            if (mode == WidgetDensityMode.Auto)
            {
                candidates.Add((widget, saving));
                anyAutoCompact |= widget.IsCompact;
            }
        }

        // Give compacted widgets some breathing room before expanding again.
        // This prevents continuous resize/compact oscillation near the width limit.
        var available = Math.Max(0, maxTotalWidth)
            - (anyAutoCompact ? Math.Max(0, hysteresisMargin) : 0);
        var overflow = Math.Max(0, projectedFullWidth - available);
        var autoCompact = new HashSet<DockWidget>();

        foreach (var (widget, saving) in candidates.OrderBy(item => item.Widget.ShrinkPriority))
        {
            if (overflow <= 0 || saving <= 0) break;
            autoCompact.Add(widget);
            overflow -= saving;
        }

        foreach (var slot in _widgets.Where(slot => slot.Widget is { Visibility: Visibility.Visible }))
        {
            var widget = slot.Widget!;
            var mode = ResolveWidgetDensity(settings, slot.Id);

            var shouldCompact = widget.SupportsCompactMode
                && (mode == WidgetDensityMode.Compact
                    || (mode == WidgetDensityMode.Auto && autoCompact.Contains(widget)));
            if (widget.IsCompact == shouldCompact) continue;
            widget.SetCompactState(shouldCompact);
            changed = true;
        }

        return changed;
    }

    private static WidgetDensityMode ResolveWidgetDensity(DockSettings settings, string widgetId)
    {
        if (settings.WidgetDensityOverrides is { Count: > 0 } overrides)
        {
            var id = DockSettingsStore.CanonicalizeWidgetId(widgetId);
            if (overrides.TryGetValue(id, out var mode))
                return mode;
            // Older snapshots may still hold case-sensitive keys with snake_case IDs.
            foreach (var (k, v) in overrides)
            {
                if (string.Equals(DockSettingsStore.CanonicalizeWidgetId(k), id,
                        StringComparison.OrdinalIgnoreCase))
                    return v;
            }
        }
        return settings.GlobalWidgetDensity;
    }

    /// <summary>Automatic density is applied as a group during layout, not one widget at a time.</summary>
    public bool TryCompactNextAutoWidget(DockSettings settings) => false;

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        HasActiveWidgets = false;
        _hoverCoordinator.Dispose();
        _timer.Stop();
        _timer.Tick -= Timer_Tick;
        foreach (var widget in _widgets.Where(item => item.Widget is not null).Select(item => item.Widget!))
        {
            widget.PreviewMouseLeftButtonDown -= Widget_PreviewMouseLeftButtonDown;
            widget.PreviewMouseLeftButtonUp -= Widget_PreviewMouseLeftButtonUp;
            widget.PreviewMouseMove -= Widget_PreviewMouseMove;
            widget.DragOver -= Widget_DragOver;
            widget.DragLeave -= Widget_DragLeave;
            widget.Drop -= Widget_Drop;
            widget.AvailabilityChanged -= Widget_AvailabilityChanged;
            widget.ContentWidthChanged -= Widget_AvailabilityChanged;
            widget.Dispose();
        }
    }
}
