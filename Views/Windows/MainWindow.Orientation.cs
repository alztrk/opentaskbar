using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>Bottom, left, or right screen edge: flips the dock's axis, insets, magnification and hide direction.</summary>
public partial class MainWindow
{
    private DockEdge? _appliedEdge;
    private DefinitionBase? _headroomDefinition;

    private DockEdge Edge => _preferences.Current.Edge;

    private bool IsVertical => Edge != DockEdge.Bottom;

    /// <summary>Scale around icon center on side docks (lift handles edge clearance) and bottom-center on bottom dock.</summary>
    private Point IconScaleOrigin => IsVertical
        ? new Point(0.5, 0.5)
        : new Point(0.5, 1.0);

    /// <summary>Launch bounce runs away from the edge: up for the bottom dock, sideways for side docks.</summary>
    private double BounceDirection => Edge == DockEdge.Left ? -1 : 1;

    private void ApplyEdge()
    {
        var edge = Edge;
        if (_appliedEdge == edge) return;
        _appliedEdge = edge;
        ResetMagnification(immediate: true);
        var vertical = edge != DockEdge.Bottom;
        DockFlyout.Edge = edge;
        TrayButton.SetEdge(edge);
        Application.Current.Resources["DockIndicatorHorizontalAlignment"] = edge switch
        {
            DockEdge.Left => HorizontalAlignment.Left,
            DockEdge.Right => HorizontalAlignment.Right,
            _ => HorizontalAlignment.Center
        };
        Application.Current.Resources["DockIndicatorVerticalAlignment"] = vertical
            ? VerticalAlignment.Center
            : VerticalAlignment.Bottom;
        Application.Current.Resources["DockIndicatorMargin"] = edge switch
        {
            DockEdge.Left => new Thickness(2, 0, 0, 0),
            DockEdge.Right => new Thickness(0, 0, 2, 0),
            _ => new Thickness(0, 0, 0, 1)
        };
        Application.Current.Resources["DockIndicatorRowHeight"] = vertical
            ? new GridLength(0)
            : new GridLength(6);
        if (_autoHideController is not null)
        {
            _autoHideController.Edge = edge;
            DockSurfaceTranslate.BeginAnimation(System.Windows.Media.TranslateTransform.XProperty, null);
            DockSurfaceTranslate.BeginAnimation(System.Windows.Media.TranslateTransform.YProperty, null);
            DockSurfaceTranslate.X = DockSurfaceTranslate.Y = 0;
        }

        // Window grid: headroom row above the shelf, or a headroom column beside it.
        DockHost.RowDefinitions.Clear();
        DockHost.ColumnDefinitions.Clear();
        var headroom = new GridLength(EffectiveHeadroom);
        var shelf = new GridLength(1, GridUnitType.Star);
        var shelfIndex = 1;
        switch (edge)
        {
            case DockEdge.Left:
                DockHost.ColumnDefinitions.Add(new ColumnDefinition { Width = shelf });
                _headroomDefinition = new ColumnDefinition { Width = headroom };
                DockHost.ColumnDefinitions.Add((ColumnDefinition)_headroomDefinition);
                shelfIndex = 0;
                break;
            case DockEdge.Right:
                _headroomDefinition = new ColumnDefinition { Width = headroom };
                DockHost.ColumnDefinitions.Add((ColumnDefinition)_headroomDefinition);
                DockHost.ColumnDefinitions.Add(new ColumnDefinition { Width = shelf });
                break;
            default:
                _headroomDefinition = new RowDefinition { Height = headroom };
                DockHost.RowDefinitions.Add((RowDefinition)_headroomDefinition);
                DockHost.RowDefinitions.Add(new RowDefinition { Height = shelf });
                break;
        }

        foreach (UIElement child in DockHost.Children)
        {
            Grid.SetRow(child, vertical ? 0 : shelfIndex);
            Grid.SetColumn(child, vertical ? shelfIndex : 0);
        }

        DockEdgeTrigger.HorizontalAlignment = edge switch
        {
            DockEdge.Left => HorizontalAlignment.Left,
            DockEdge.Right => HorizontalAlignment.Right,
            _ => HorizontalAlignment.Stretch
        };
        DockEdgeTrigger.VerticalAlignment = vertical ? VerticalAlignment.Stretch : VerticalAlignment.Bottom;
        DockEdgeTrigger.Width = vertical ? DockMetrics.EdgeTriggerHeight : double.NaN;
        DockEdgeTrigger.Height = vertical ? double.NaN : DockMetrics.EdgeTriggerHeight;

        var orientation = vertical ? Orientation.Vertical : Orientation.Horizontal;

        // Flip the outer layout grid between 4 horizontal columns (Bottom) and 4 vertical rows (Left/Right)
        // so lead shell controls, apps, and trailing controls stack into a single vertical strip.
        DockLayoutRoot.RowDefinitions.Clear();
        DockLayoutRoot.ColumnDefinitions.Clear();
        if (vertical)
        {
            DockLayoutRoot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            DockLayoutRoot.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            DockLayoutRoot.RowDefinitions.Add(new RowDefinition { Height = IsFullWidth ? new GridLength(1, GridUnitType.Star) : GridLength.Auto });
            DockLayoutRoot.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            DockLayoutRoot.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

            Grid.SetRow(DockLeadControls, 0); Grid.SetColumn(DockLeadControls, 0);
            Grid.SetRow(DockScrollViewer, 1); Grid.SetColumn(DockScrollViewer, 0);
            Grid.SetRow(DockTrailControls, 2); Grid.SetColumn(DockTrailControls, 0);
            Grid.SetRow(WidgetIsland, 3); Grid.SetColumn(WidgetIsland, 0);

            DockLayoutRoot.MinWidth = 0;
            DockScrollViewer.MaxWidth = double.PositiveInfinity;
            DockScrollViewer.HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled;
            DockScrollViewer.VerticalScrollBarVisibility = ScrollBarVisibility.Hidden;
            DockScrollViewer.ScrollToHorizontalOffset(0);
            DockScrollViewer.ScrollToVerticalOffset(0);
            ScrollPrevOverlay.Visibility = Visibility.Collapsed;
            ScrollNextOverlay.Visibility = Visibility.Collapsed;
        }
        else
        {
            DockLayoutRoot.ColumnDefinitions.Add(LeadColumn);
            DockLayoutRoot.ColumnDefinitions.Add(AppsColumn);
            DockLayoutRoot.ColumnDefinitions.Add(TrailColumn);
            DockLayoutRoot.ColumnDefinitions.Add(WidgetColumn);

            Grid.SetRow(DockLeadControls, 0); Grid.SetColumn(DockLeadControls, 0);
            Grid.SetRow(DockScrollViewer, 0); Grid.SetColumn(DockScrollViewer, 1);
            Grid.SetRow(DockTrailControls, 0); Grid.SetColumn(DockTrailControls, 2);
            Grid.SetRow(WidgetIsland, 0); Grid.SetColumn(WidgetIsland, 3);

            DockLayoutRoot.MinHeight = 0;
            DockScrollViewer.MaxHeight = double.PositiveInfinity;
            DockScrollViewer.HorizontalScrollBarVisibility = ScrollBarVisibility.Hidden;
            DockScrollViewer.VerticalScrollBarVisibility = ScrollBarVisibility.Disabled;
            DockScrollViewer.ScrollToVerticalOffset(0);
        }

        DockLeadControls.Orientation = orientation;
        DockLeadControls.HorizontalAlignment = vertical ? HorizontalAlignment.Center : HorizontalAlignment.Left;
        DockLeadControls.VerticalAlignment = vertical ? VerticalAlignment.Top : VerticalAlignment.Center;
        DockLeadControls.Margin = vertical ? new Thickness(0, 3, 0, 0) : new Thickness(3, 0, 0, 0);

        DockTrailControls.Orientation = orientation;
        DockTrailControls.HorizontalAlignment = vertical ? HorizontalAlignment.Center : HorizontalAlignment.Left;
        DockTrailControls.VerticalAlignment = vertical ? VerticalAlignment.Bottom : VerticalAlignment.Center;
        DockTrailControls.Margin = vertical ? new Thickness(0, 0, 0, 3) : new Thickness(0, 0, 3, 0);

        ShowDesktopButton.Width = vertical ? double.NaN : 6;
        ShowDesktopButton.Height = vertical ? 6 : double.NaN;
        ShowDesktopButton.HorizontalAlignment = vertical ? HorizontalAlignment.Stretch : HorizontalAlignment.Center;
        ShowDesktopButton.VerticalAlignment = vertical ? VerticalAlignment.Center : VerticalAlignment.Stretch;
        ShowDesktopButton.Margin = vertical ? new Thickness(0, 3, 0, 0) : new Thickness(3, 0, 0, 0);

        DockContents.Orientation = orientation;
        DockContents.HorizontalAlignment = vertical ? HorizontalAlignment.Center : HorizontalAlignment.Left;
        DockContents.VerticalAlignment = vertical ? VerticalAlignment.Top : VerticalAlignment.Center;
        var panel = new ItemsPanelTemplate(new FrameworkElementFactory(typeof(StackPanel)));
        panel.VisualTree.SetValue(StackPanel.OrientationProperty, orientation);
        PinnedApps.ItemsPanel = panel;
        RunningApps.ItemsPanel = panel;
        StackItems.ItemsPanel = panel;
        DockWidgets.ItemsPanel = panel;
        foreach (var divider in new[] { StartDivider, RunningAppsDivider, ExtrasDivider, WidgetDivider,
            NativeGroupDivider, TrayGroupDivider })
        {
            divider.Width = vertical ? 22 : 1;
            divider.Height = vertical ? 1 : 22;
            divider.Margin = vertical ? new Thickness(0, 4, 0, 4) : new Thickness(4, 0, 4, 0);
        }

        // Allow magnified icons to expand past the dock edge on Bottom, Left, and Right docks alike.
        DockScrollViewer.DockEdge = edge;
        DockScrollViewer.HoverHeadroom = DockMetrics.Headroom;
        WidgetIsland.Visibility = vertical ? Visibility.Collapsed : Visibility.Visible;
        DockSheen.Visibility = vertical ? Visibility.Collapsed : Visibility.Visible;
        LeftResizeThumb.Visibility = RightResizeThumb.Visibility = vertical ? Visibility.Collapsed : Visibility.Visible;
        foreach (var button in GetAllDockButtons())
        {
            if (GetButtonIcon(button) is { } icon) icon.RenderTransformOrigin = IconScaleOrigin;
        }

        RebuildDockOrder();
        // Apply size profile BEFORE switching _replacementSession.Edge so the first AppBar reservation
        // immediately claims the full vertical shelf width instead of the stale horizontal height.
        ApplySizeProfile();
        if (_replacementSession is not null) _replacementSession.Edge = edge;
        ApplyDockWidthMode();
        ApplyLayoutMode();
    }

    /// <summary>Updates the headroom row/column in place when the external top gap setting changes.</summary>
    private void ApplyHeadroom()
    {
        if (_headroomDefinition is null) return;
        var headroom = new GridLength(EffectiveHeadroom);
        if (_headroomDefinition is RowDefinition row) row.Height = headroom;
        else if (_headroomDefinition is ColumnDefinition column) column.Width = headroom;
    }

    /// <summary>Side docks: honors Auto/Pixels/Percent/Fill height modes and enables vertical scrolling on overflow.</summary>
    private void ResizeSideDockToContent()
    {
        var available = _dockArea.Height > 0 ? _dockArea.Height : DisplayService.GetTargetDisplay(_preferences.Current.TargetDisplay).Height;
        var marginV = DockSurface.Margin.Top + DockSurface.Margin.Bottom;
        var maximumSurfaceHeight = Math.Max(1, available - marginV);
        var verticalChrome = DockFrame.Padding.Top + DockFrame.Padding.Bottom + 4;
        var shellCount = (StartButtonShown ? 1 : 0)
            + (_preferences.Current.ShowSearchButton ? 1 : 0)
            + (_preferences.Current.ShowTaskViewButton ? 1 : 0)
            + (_preferences.Current.ShowTrayButton ? 1 : 0);
        var count = Math.Max(1, shellCount + VisiblePinnedCount + _runningApps.Count + ExtraButtonCount);

        DockSurface.BeginAnimation(WidthProperty, null);
        DockLayoutRoot.MinWidth = 0;
        DockScrollViewer.MaxWidth = double.PositiveInfinity;
        ScrollPrevOverlay.Visibility = Visibility.Collapsed;
        ScrollNextOverlay.Visibility = Visibility.Collapsed;
        DockSurface.Width = double.NaN;

        const double verticalExtraWidth = 8;
        if (IsFullWidth || IsEdgeToEdge)
        {
            Application.Current.Resources["DockCellSize"] = Profile.Cell + verticalExtraWidth;
            Application.Current.Resources["DockCellHeight"] = Profile.Cell;
            DockLayoutRoot.MinHeight = Math.Max(0, maximumSurfaceHeight - verticalChrome);
            DockScrollViewer.MaxHeight = Math.Max(Profile.Cell, maximumSurfaceHeight - verticalChrome);
            DockSurface.Height = double.NaN;
            return;
        }

        if (_dockWidthOverride is { } fixedHeight)
        {
            var targetHeight = Math.Clamp(fixedHeight, Math.Min(MinimumDockSurfaceWidth, maximumSurfaceHeight), maximumSurfaceHeight);
            Application.Current.Resources["DockCellSize"] = Profile.Cell + verticalExtraWidth;
            Application.Current.Resources["DockCellHeight"] = Profile.Cell;
            DockLayoutRoot.MinHeight = 0;
            DockScrollViewer.MaxHeight = Math.Max(Profile.Cell, targetHeight - verticalChrome);
            DockSurface.Height = targetHeight;
            return;
        }

        var frame = verticalChrome + marginV + 24;
        Application.Current.Resources["DockCellSize"] = Profile.Cell + verticalExtraWidth;
        Application.Current.Resources["DockCellHeight"] = Math.Min(Profile.Cell,
            Math.Max(DockMetrics.MinCellSize, (available - frame) / count));
        DockLayoutRoot.MinHeight = 0;
        DockScrollViewer.MaxHeight = Math.Max(Profile.Cell, maximumSurfaceHeight - verticalChrome);
        DockSurface.Height = double.NaN;
    }
}
