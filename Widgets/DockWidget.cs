using System.ComponentModel;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Base for dock widgets. The coordinator activates a widget while it is enabled in settings and ticks it once
/// per second; inactive widgets must not poll or hold system subscriptions.
/// </summary>
public class DockWidget : UserControl, IDisposable
{
    private bool _hasData = true;
    private bool _disposed;

    public DockWidget()
    {
        Visibility = Visibility.Collapsed;
        SetResourceReference(ForegroundProperty, "TextPrimary");
        Focusable = false;
        RenderTransform = new TransformGroup
        {
            Children = { new ScaleTransform(), new TranslateTransform() }
        };
        RenderTransformOrigin = new Point(0.5, 0.5);
    }

    public Action<string>? ReportError { get; set; }

    /// <summary>Raised when the widget appears or disappears, so the dock can refit its widget area.</summary>
    public event EventHandler? AvailabilityChanged;

    /// <summary>
    /// Raised when the widget's internal content changes its measured width without changing the widget's own
    /// visibility, e.g. a system-stat meter becoming available or unavailable. The dock must refit for this too.
    /// </summary>
    public event EventHandler? ContentWidthChanged;

    protected bool IsActive { get; private set; }
    protected DockSettings Settings { get; private set; } = new();
    public virtual bool SupportsCompactMode => false;
    public virtual int ShrinkPriority => 100;
    public virtual double FullWidthEstimate => Math.Max(0, DesiredSize.Width);
    public virtual double CompactWidthEstimate => FullWidthEstimate;
    public bool IsCompact { get; private set; }
    public bool IsHoverExpanded { get; private set; }
    public bool IsHoverFlyoutOpen { get; internal set; }

    internal void SetCompactState(bool compact)
    {
        if (!SupportsCompactMode) compact = false;
        if (IsCompact == compact) return;
        IsCompact = compact;
        if (!compact && IsHoverExpanded)
        {
            IsHoverExpanded = false;
            OnHoverExpandedChanged(false);
        }
        OnCompactChanged(compact);
    }

    internal void SetHoverExpanded(bool expanded)
    {
        if (!SupportsCompactMode || !IsCompact) expanded = false;
        if (IsHoverExpanded == expanded) return;
        IsHoverExpanded = expanded;
        OnHoverExpandedChanged(expanded);
    }

    public virtual UIElement? BuildHoverFlyoutContent() => null;

    protected virtual void OnCompactChanged(bool compact) { }
    protected virtual void OnHoverExpandedChanged(bool expanded) { }

    /// <summary>False hides the widget while active, e.g. media controls with no playing session.</summary>
    protected bool HasData
    {
        get => _hasData;
        set
        {
            _hasData = value;
            UpdateVisibility();
        }
    }

    public void Activate()
    {
        if (IsActive || _disposed) return;
        IsActive = true;
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        UpdateVisibility();
        OnActivated();
    }

    public void Deactivate()
    {
        if (!IsActive) return;
        IsActive = false;
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        UpdateVisibility();
        OnDeactivated();
    }

    public void ApplySettings(DockSettings settings)
    {
        Settings = settings;
        OnSettingsChanged();
    }

    public void Tick(DateTime now)
    {
        if (IsActive) OnTick(now);
    }

    /// <summary>
    /// Renders a single frame for the Settings catalog preview without activating the widget.
    /// No background polling or service subscriptions are started. Widgets that fetch remote data
    /// override OnRenderPreview so they only paint their placeholder state.
    /// </summary>
    public void RenderPreview()
    {
        if (_disposed || IsActive) return;
        OnRenderPreview();
    }

    protected virtual void OnRenderPreview() => OnTick(DateTime.Now);

    protected virtual void OnActivated() { }
    protected virtual void OnDeactivated() { }
    protected virtual void OnDisposed() { }
    protected virtual void OnSettingsChanged() { }
    protected virtual void OnLanguageChanged() { }
    protected virtual void OnTick(DateTime now) { }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (IsActive) OnLanguageChanged();
    }

    protected void Report(string? error)
    {
        if (error is not null) ReportError?.Invoke(error);
    }

    /// <summary>Notify the host that the widget's measured width changed.</summary>
    protected void RaiseContentWidthChanged() => ContentWidthChanged?.Invoke(this, EventArgs.Empty);

    private void UpdateVisibility()
    {
        var visibility = IsActive && _hasData ? Visibility.Visible : Visibility.Collapsed;
        if (Visibility == visibility) return;
        Visibility = visibility;
        AvailabilityChanged?.Invoke(this, EventArgs.Empty);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Deactivate();
        OnDisposed();
        GC.SuppressFinalize(this);
    }
}
