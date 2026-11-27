using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media.Animation;
using System.Windows.Media.Effects;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

public partial class MainWindow
{
    private const uint SwpNoOwnerZOrder = 0x0200;

    internal const double DefaultDockShadowMarginPerSide = 40.0; // BlurRadius (36) + ShadowDepth (4)

    internal readonly record struct LayeredWindowBounds(double Offset, double Extent, bool IsShrunk);

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeWindowRect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll", EntryPoint = "GetWindowRect", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetLayeredWindowRect(nint windowHandle, out NativeWindowRect rect);

    private bool _updatingLayeredWindowBounds;
    private bool _layeredWindowShrunk;
    private bool _layeredWindowSyncQueued;
    private bool _layeredWindowPreferencesSubscribed;
    private bool _dockDoubleClickAnimating;
    private DispatcherTimer? _layeredWindowShrinkSettleTimer;

    private double DockHorizontalChrome => DockFrame.Padding.Left + DockFrame.Padding.Right
        + DockFrame.BorderThickness.Left + DockFrame.BorderThickness.Right
        + DockScrollViewer.ContentInset.Left + DockScrollViewer.ContentInset.Right;

    private double MinimumDockSurfaceWidth => (double)FindResource("DockCellSize") + DockHorizontalChrome;

    /// <summary>
    /// Computes the horizontal (or vertical) shadow bleed margin on each open side of the dock surface so
    /// shrinking the transparent layered host window never clips the DropShadowEffect blur.
    /// </summary>
    internal static double ComputeShadowMarginPerSide(Effect? effect)
    {
        if (effect is DropShadowEffect shadow)
        {
            if (shadow.Opacity <= 0.001) return 0.0;
            return Math.Ceiling(Math.Max(0.0, shadow.BlurRadius) + Math.Max(0.0, shadow.ShadowDepth));
        }

        return effect is null ? 0.0 : DefaultDockShadowMarginPerSide;
    }

    /// <summary>
    /// Computes the tight transparent layered-window bounds along the dock axis for floating mode
    /// (Issue #128 Finding #3a). Full-width mode and auto-hide keep the full monitor span so the
    /// edge-to-edge bar and auto-hide edge trigger behave identically.
    /// </summary>
    internal static LayeredWindowBounds ComputeFloatingLayeredWindowBounds(
        double monitorOffset,
        double monitorExtent,
        double dockSurfaceExtent,
        double surfaceMarginStart,
        double surfaceMarginEnd,
        double magnificationHeadroom,
        double shadowMarginPerSide,
        DockPosition position,
        bool isFullWidth,
        bool preserveFullEdgeForAutoHide)
    {
        if (!double.IsFinite(monitorOffset) || !double.IsFinite(monitorExtent) || monitorExtent <= 0)
        {
            return new LayeredWindowBounds(double.IsFinite(monitorOffset) ? monitorOffset : 0.0,
                double.IsFinite(monitorExtent) ? Math.Max(0.0, monitorExtent) : 0.0,
                IsShrunk: false);
        }

        if (isFullWidth || preserveFullEdgeForAutoHide || !double.IsFinite(dockSurfaceExtent) || dockSurfaceExtent <= 0)
        {
            return new LayeredWindowBounds(monitorOffset, monitorExtent, IsShrunk: false);
        }

        var startMargin = double.IsFinite(surfaceMarginStart) ? Math.Max(0.0, surfaceMarginStart) : 0.0;
        var endMargin = double.IsFinite(surfaceMarginEnd) ? Math.Max(0.0, surfaceMarginEnd) : 0.0;
        var magHeadroom = double.IsFinite(magnificationHeadroom) ? Math.Max(0.0, magnificationHeadroom) : 0.0;
        var shadowPerSide = double.IsFinite(shadowMarginPerSide) ? Math.Max(0.0, shadowMarginPerSide) : 0.0;
        var shadowTotal = position == DockPosition.Center ? shadowPerSide * 2.0 : shadowPerSide;

        var desiredExtent = Math.Ceiling(dockSurfaceExtent + startMargin + endMargin + magHeadroom + shadowTotal);
        if (desiredExtent >= monitorExtent - 2.0)
        {
            return new LayeredWindowBounds(monitorOffset, monitorExtent, IsShrunk: false);
        }

        var offset = position switch
        {
            DockPosition.Left => monitorOffset,
            DockPosition.Right => monitorOffset + monitorExtent - desiredExtent,
            _ => monitorOffset + (monitorExtent - desiredExtent) / 2.0
        };
        offset = Math.Clamp(offset, monitorOffset, monitorOffset + monitorExtent - desiredExtent);
        return new LayeredWindowBounds(offset, desiredExtent, IsShrunk: true);
    }

    private void EnsureLayeredWindowPreferencesHook()
    {
        if (_layeredWindowPreferencesSubscribed) return;
        _layeredWindowPreferencesSubscribed = true;
        _preferences.Changed += LayeredWindow_PreferencesChanged;
        Closed += (_, _) =>
        {
            _preferences.Changed -= LayeredWindow_PreferencesChanged;
            _layeredWindowShrinkSettleTimer?.Stop();
        };
    }

    private void LayeredWindow_PreferencesChanged(object? sender, EventArgs e)
    {
        if (_closed) return;
        ScheduleFloatingLayeredWindowSync();
    }

    private void Window_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        EnsureLayeredWindowPreferencesHook();
        if (_updatingLayeredWindowBounds || !IsLoaded) return;
        ScheduleFloatingLayeredWindowSync();
    }

    private void DockSurface_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        EnsureLayeredWindowPreferencesHook();
        if (_updatingLayeredWindowBounds || !IsLoaded) return;
        if (_magnificationLayoutActive || _magnificationRendering) return;
        ScheduleFloatingLayeredWindowSync();
    }

    internal void ScheduleFloatingLayeredWindowSync()
    {
        if (_closed || _layeredWindowSyncQueued) return;
        _layeredWindowSyncQueued = true;
        Dispatcher.BeginInvoke(DispatcherPriority.Loaded, new Action(() =>
        {
            _layeredWindowSyncQueued = false;
            SyncFloatingLayeredWindowBounds();
        }));
    }

    private void SchedulePostAnimationLayeredWindowSync()
    {
        _layeredWindowShrinkSettleTimer ??= CreateLayeredWindowShrinkSettleTimer();
        _layeredWindowShrinkSettleTimer.Stop();
        _layeredWindowShrinkSettleTimer.Start();
    }

    private DispatcherTimer CreateLayeredWindowShrinkSettleTimer()
    {
        var timer = new DispatcherTimer(DispatcherPriority.Loaded) { Interval = TimeSpan.FromMilliseconds(150) };
        timer.Tick += (_, _) =>
        {
            timer.Stop();
            if (_closed) return;
            if (!double.IsNaN(_dockWidthAnimationTarget) || _dockDoubleClickAnimating)
            {
                timer.Start();
                return;
            }
            SyncFloatingLayeredWindowBounds();
        };
        return timer;
    }

    private void SyncFloatingLayeredWindowBounds(bool forceFullMonitorSpan = false)
    {
        EnsureLayeredWindowPreferencesHook();
        if (_closed || !IsLoaded || _updatingLayeredWindowBounds || _resizingDock) return;
        if (!forceFullMonitorSpan && (_magnificationLayoutActive || _magnificationRendering)) return;
        if (!forceFullMonitorSpan && (LeftResizeThumb.IsDragging || RightResizeThumb.IsDragging)) return;

        var display = GetDockDisplay();
        var area = display.Bounds;
        var vertical = IsVertical;
        var monitorOffset = vertical ? area.Top : area.Left;
        var monitorExtent = vertical ? area.Height : area.Width;
        if (monitorExtent <= 0) return;

        var margin = DockSurface.Margin;
        var isWidthAnimating = !vertical && (!double.IsNaN(_dockWidthAnimationTarget) || _dockDoubleClickAnimating);
        double dockSurfaceExtent;
        if (vertical)
        {
            dockSurfaceExtent = !double.IsNaN(DockSurface.Height) && DockSurface.Height > 0
                ? DockSurface.Height
                : DockSurface.ActualHeight;
        }
        else
        {
            var baseWidth = !double.IsNaN(DockSurface.Width) && DockSurface.Width > 0
                ? DockSurface.Width
                : DockSurface.ActualWidth;
            dockSurfaceExtent = !double.IsNaN(_dockWidthAnimationTarget)
                ? Math.Max(baseWidth, _dockWidthAnimationTarget)
                : baseWidth;
        }

        var magnificationHeadroom = _preferences.Current.MagnificationEnabled
            && _preferences.Current.EffectiveAnimationsEnabled
            && _dockWidthOverride is null
            && _preferences.Current.MagnificationExpandsDock
            ? DockFisheyeLayout.PeakExpansionNeed(Profile.Cell, DockMetrics.HoverScale)
            : 0.0;
        var shadowMarginPerSide = ComputeShadowMarginPerSide(DockShadowLayer.Effect);
        var preserveFullEdgeForAutoHide = forceFullMonitorSpan || _preferences.Current.AutoHide != AutoHideMode.Disabled;

        var bounds = ComputeFloatingLayeredWindowBounds(
            monitorOffset,
            monitorExtent,
            dockSurfaceExtent,
            vertical ? margin.Top : margin.Left,
            vertical ? margin.Bottom : margin.Right,
            magnificationHeadroom,
            shadowMarginPerSide,
            _dockPosition,
            IsFullWidth,
            preserveFullEdgeForAutoHide);

        if (!bounds.IsShrunk && !_layeredWindowShrunk) return;

        // Avoid calling SetWindowPos on every frame of a shrink animation: expand immediately if needed,
        // and defer shrinking until the animation finishes.
        if (isWidthAnimating && bounds.IsShrunk && _layeredWindowShrunk)
        {
            var currentExtent = ActualWidth > 0 ? ActualWidth : Width;
            if (double.IsFinite(currentExtent) && bounds.Extent <= currentExtent + 0.5)
            {
                SchedulePostAnimationLayeredWindowSync();
                return;
            }
        }

        var dpi = display.Dpi > 0 ? display.Dpi : 96u;
        var scale = dpi / 96.0;
        var applied = false;

        _updatingLayeredWindowBounds = true;
        try
        {
            if (_source is { Handle: not 0 } source && GetLayeredWindowRect(source.Handle, out var rect))
            {
                var currentWidthPx = rect.Right - rect.Left;
                var currentHeightPx = rect.Bottom - rect.Top;
                if (vertical)
                {
                    var targetTopPx = (int)Math.Round(bounds.Offset * scale);
                    var targetHeightPx = Math.Max(1, (int)Math.Round(bounds.Extent * scale));
                    if (Math.Abs(rect.Top - targetTopPx) >= 1 || Math.Abs(currentHeightPx - targetHeightPx) >= 1)
                    {
                        SetWindowPos(source.Handle, 0, rect.Left, targetTopPx, currentWidthPx, targetHeightPx,
                            SwpNoZOrder | SwpNoActivate | SwpNoOwnerZOrder);
                        applied = true;
                    }
                }
                else
                {
                    var targetLeftPx = (int)Math.Round(bounds.Offset * scale);
                    var targetWidthPx = Math.Max(1, (int)Math.Round(bounds.Extent * scale));
                    if (Math.Abs(rect.Left - targetLeftPx) >= 1 || Math.Abs(currentWidthPx - targetWidthPx) >= 1)
                    {
                        SetWindowPos(source.Handle, 0, targetLeftPx, rect.Top, targetWidthPx, currentHeightPx,
                            SwpNoZOrder | SwpNoActivate | SwpNoOwnerZOrder);
                        applied = true;
                    }
                }
            }
            else if (vertical)
            {
                if (Math.Abs(Top - bounds.Offset) >= 0.5 || Math.Abs(Height - bounds.Extent) >= 0.5)
                {
                    Top = bounds.Offset;
                    Height = bounds.Extent;
                    applied = true;
                }
            }
            else
            {
                if (Math.Abs(Left - bounds.Offset) >= 0.5 || Math.Abs(Width - bounds.Extent) >= 0.5)
                {
                    Left = bounds.Offset;
                    Width = bounds.Extent;
                    applied = true;
                }
            }

            _layeredWindowShrunk = bounds.IsShrunk;
        }
        finally
        {
            _updatingLayeredWindowBounds = false;
        }

        if (applied)
        {
            InvalidateMagnificationTargets();
        }
    }

    private void DockResizeThumb_DragStarted(object sender, DragStartedEventArgs e)
    {
        _widgetPanel.Close(restoreFocus: false);
        _resizingFromLeft = ReferenceEquals(sender, LeftResizeThumb);
        ResetMagnification(immediate: true);
        if (_layeredWindowShrunk)
        {
            SyncFloatingLayeredWindowBounds(forceFullMonitorSpan: true);
        }
    }

    private void DockResizeThumb_DragDelta(object sender, DragDeltaEventArgs e)
    {
        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : GetDockDisplay().Width;
        var margin = DockSurface.Margin;
        var maximumSurfaceWidth = Math.Max(1, availableWidth - margin.Left - margin.Right);
        var minimumSurfaceWidth = Math.Min(MinimumDockSurfaceWidth, maximumSurfaceWidth);

        double delta;
        if (_dockPosition == DockPosition.Center)
        {
            delta = (_resizingFromLeft ? -e.HorizontalChange : e.HorizontalChange) * 2;
        }
        else if (_dockPosition == DockPosition.Left)
        {
            delta = _resizingFromLeft ? -e.HorizontalChange : e.HorizontalChange;
        }
        else
        {
            delta = _resizingFromLeft ? -e.HorizontalChange : e.HorizontalChange;
        }

        var nextWidth = Math.Clamp(DockSurface.ActualWidth + delta, minimumSurfaceWidth, maximumSurfaceWidth);
        _dockWidthOverride = nextWidth;
        DockSurface.Width = nextWidth;
        DockSurfaceTranslate.X = 0;
        ResizeDockToContent();
        DockSurface.UpdateLayout();

        if (_replacementSession?.IsActive == true)
        {
            _replacementSession.RefreshPosition();
        }
        else
        {
            PositionDock();
        }
    }

    private void DockResizeThumb_DragCompleted(object sender, DragCompletedEventArgs e)
    {
        DockSurfaceTranslate.X = 0;
        if (_replacementSession?.IsActive == true)
        {
            _replacementSession.RefreshPosition();
        }
        else
        {
            PositionDock();
        }
        ScheduleFloatingLayeredWindowSync();
    }

    private void DockResizeThumb_MouseDoubleClick(object sender, MouseButtonEventArgs e)
    {
        e.Handled = true;
        _dockWidthOverride = null;
        var contentWidth = DockScrollViewer.MeasureContentWidth(DockContents);
        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : GetDockDisplay().Width;
        var margin = DockSurface.Margin;
        var maximumSurfaceWidth = Math.Max(1, availableWidth - margin.Left - margin.Right);
        var hasWidgets = WidgetStrip.Children.OfType<UIElement>().Any(child => child.Visibility == Visibility.Visible);
        var widgetsWidth = hasWidgets ? WidgetIsland.DesiredSize.Width + WidgetIsland.Margin.Left + WidgetIsland.Margin.Right : 0;
        var desiredWidth = Math.Clamp(Math.Ceiling(contentWidth + widgetsWidth + DockHorizontalChrome),
            Math.Min(MinimumDockSurfaceWidth, maximumSurfaceWidth), maximumSurfaceWidth);

        DockSurfaceTranslate.X = 0;
        if (!_preferences.Current.EffectiveAnimationsEnabled)
        {
            DockSurface.Width = desiredWidth;
            ResizeDockToContent();
            if (_replacementSession?.IsActive == true) _replacementSession.RefreshPosition();
            else PositionDock();
            return;
        }

        var anim = new DoubleAnimation
        {
            From = DockSurface.ActualWidth,
            To = desiredWidth,
            Duration = TimeSpan.FromMilliseconds(220),
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        };
        _dockDoubleClickAnimating = true;
        anim.Completed += (_, _) =>
        {
            _dockDoubleClickAnimating = false;
            DockSurface.BeginAnimation(WidthProperty, null);
            DockSurface.Width = desiredWidth;
            ResizeDockToContent();
            if (_replacementSession?.IsActive == true) _replacementSession.RefreshPosition();
            else PositionDock();
            ScheduleFloatingLayeredWindowSync();
        };
        DockSurface.BeginAnimation(WidthProperty, anim);
    }

    private void DockScrollViewer_ScrollChanged(object sender, ScrollChangedEventArgs e)
    {
        if (e.HorizontalChange == 0 && e.VerticalChange == 0) return;
        _widgetPanel.Close(restoreFocus: false);

        // This is the entry to the documented feedback chain:
        //   ScrollChanged -> ResetMagnification -> deferred ResizeDockToContent -> FitDockToContent.
        // Opening a gesture scope here and closing it only after the deferred resize completes keeps
        // DockScrollViewer.SetContentOverflow(false) from resetting the strip to offset 0 on a transient
        // "no overflow" measurement taken mid-chain. If a resize is already running, the ScrollChanged was
        // caused by that resize's own offset write, so stop here instead of starting another loop pass.
        BeginScrollGestureScope();
        var before = DockScrollViewer.HorizontalOffset;
        if (_resizingDock)
        {
            ShellTelemetry.Event("ScrollLayout", ("phase", "scroll-reentrant-skipped"),
                ("before", before), ("target", DockScrollViewer.DesiredHorizontalOffset),
                ("resizePending", _deferredDockResize));
            EndScrollGestureScope("resize-in-progress");
            return;
        }

        // Smooth scrolling fires this once per frame; the 90ms reset timer coalesces the frames into a
        // single magnification reset, and the deferred resize is flushed at most once per gesture.
        ShellTelemetry.Event("ScrollLayout", ("phase", "scroll-reset-deferred"),
            ("before", before), ("target", DockScrollViewer.DesiredHorizontalOffset),
            ("resizePending", _deferredDockResize), ("gesture", DockScrollViewer.IsGestureActive));
        ResetMagnificationAfterScroll();
    }
}
