using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media.Animation;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

public partial class MainWindow
{
    private double DockHorizontalChrome => DockFrame.Padding.Left + DockFrame.Padding.Right
        + DockFrame.BorderThickness.Left + DockFrame.BorderThickness.Right
        + DockScrollViewer.ContentInset.Left + DockScrollViewer.ContentInset.Right;

    private double MinimumDockSurfaceWidth => (double)FindResource("DockCellSize") + DockHorizontalChrome;

    private void DockResizeThumb_DragStarted(object sender, DragStartedEventArgs e)
    {
        _widgetPanel.Close(restoreFocus: false);
        _resizingFromLeft = ReferenceEquals(sender, LeftResizeThumb);
        ResetMagnification(immediate: true);
    }

    private void DockResizeThumb_DragDelta(object sender, DragDeltaEventArgs e)
    {
        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay).Width;
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
    }

    private void DockResizeThumb_MouseDoubleClick(object sender, MouseButtonEventArgs e)
    {
        e.Handled = true;
        _dockWidthOverride = null;
        var contentWidth = DockScrollViewer.MeasureContentWidth(DockContents);
        var availableWidth = _dockArea.Width > 0 ? _dockArea.Width : DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay).Width;
        var margin = DockSurface.Margin;
        var maximumSurfaceWidth = Math.Max(1, availableWidth - margin.Left - margin.Right);
        var hasWidgets = WidgetStrip.Children.OfType<UIElement>().Any(child => child.Visibility == Visibility.Visible);
        var widgetsWidth = hasWidgets ? WidgetIsland.DesiredSize.Width + WidgetIsland.Margin.Left + WidgetIsland.Margin.Right : 0;
        var desiredWidth = Math.Clamp(Math.Ceiling(contentWidth + widgetsWidth + DockHorizontalChrome),
            Math.Min(MinimumDockSurfaceWidth, maximumSurfaceWidth), maximumSurfaceWidth);

        DockSurfaceTranslate.X = 0;
        if (!_preferences.Current.AnimationsEnabled)
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
        anim.Completed += (_, _) =>
        {
            DockSurface.BeginAnimation(WidthProperty, null);
            DockSurface.Width = desiredWidth;
            ResizeDockToContent();
            if (_replacementSession?.IsActive == true) _replacementSession.RefreshPosition();
            else PositionDock();
        };
        DockSurface.BeginAnimation(WidthProperty, anim);
    }

    private void DockScrollViewer_ScrollChanged(object sender, ScrollChangedEventArgs e)
    {
        if (e.HorizontalChange == 0) return;
        _widgetPanel.Close(restoreFocus: false);
        ResetMagnification(immediate: true);
    }
}
