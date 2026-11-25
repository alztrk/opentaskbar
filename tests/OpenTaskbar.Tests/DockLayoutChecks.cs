using System.Collections.ObjectModel;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using OpenTaskbar;
using OpenTaskbar.Models;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using OpenTaskbar.Services;
using OpenTaskbar.Views;
using OpenTaskbar.Widgets;
using System.Windows.Threading;

internal static class DockLayoutChecks
{
    public static int Run(Application app)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        // Edge-to-edge fill mode must be a valid width mode so users can span the dock across the whole screen.
        Check(Enum.IsDefined(DockWidthMode.Fill), "DockWidthMode.Fill must be a defined width mode.");

        // External top/bottom gaps: pixels and percent resolve correctly, zero is allowed, negatives are clamped.
        Check(DockMetrics.ResolveGap(DockGapUnit.Pixels, 12, 0, 1920) == 12, "Pixel gap must resolve to its pixel value.");
        Check(DockMetrics.ResolveGap(DockGapUnit.Percent, 0, 25, 1920) == 480, "Percent gap must resolve to a share of the screen extent.");
        Check(DockMetrics.ResolveGap(DockGapUnit.Pixels, 0, 0, 1920) == 0, "Gap must allow zero (flush against the edge).");
        Check(DockMetrics.ResolveGap(DockGapUnit.Percent, 0, 0, 1920) == 0, "Percent gap must allow zero.");
        Check(DockMetrics.ResolveGap(DockGapUnit.Pixels, -5, 0, 1920) == 0, "Negative gap must clamp to zero.");

        // Height modes: Auto keeps the profile shelf, Pixels/Percent pin the visible bar and reserve the shelf
        // around it, Fill lets the bar span the whole reserved strip (bottom gap ignored, top gap preserved).
        Check(Enum.IsDefined(DockHeightMode.Pixels) && Enum.IsDefined(DockHeightMode.Percent) && Enum.IsDefined(DockHeightMode.Fill),
            "All dock height modes must be defined.");
        var compactProfile = DockMetrics.For(DockSize.Compact);
        var autoHeight = MainWindow.ComputeShelfGeometry(compactProfile, false, 1, 6, 0);
        Check(double.IsNaN(autoHeight.SurfaceHeight) && !autoHeight.Fill && Math.Abs(autoHeight.Shelf - 52) < 0.01,
            "Auto height must keep the existing content-sized bar and shelf formula.");
        var pixelHeight = MainWindow.ComputeShelfGeometry(compactProfile, false, 1, 6, 0, DockHeightMode.Pixels, 80, 0, 1080);
        Check(Math.Abs(pixelHeight.SurfaceHeight - 80) < 0.01 && pixelHeight.Shelf >= 88 - 0.01,
            "Pixels height must size the visible bar and reserve the shelf plus the gap around it.");
        var percentHeight = MainWindow.ComputeShelfGeometry(compactProfile, false, 1, 0, 0, DockHeightMode.Percent, 0, 5, 1000);
        Check(Math.Abs(percentHeight.SurfaceHeight - 50) < 0.01,
            "Percent height must resolve to a share of the monitor extent.");
        var fillHeight = MainWindow.ComputeShelfGeometry(compactProfile, false, 1, 6, 0, DockHeightMode.Fill);
        Check(fillHeight.Fill && Math.Abs(fillHeight.SurfaceHeight - fillHeight.Shelf) < 0.01,
            "Fill height must stretch the visible bar across the whole reserved shelf.");
        var fillTopGap = MainWindow.ComputeShelfGeometry(compactProfile, false, 1, 6, 10, DockHeightMode.Fill);
        Check(Math.Abs((fillTopGap.Shelf - fillHeight.Shelf) - 10) < 0.01
            && Math.Abs(fillTopGap.SurfaceHeight - fillHeight.SurfaceHeight) < 0.01,
            "The top gap must still grow the reserved shelf in Fill mode without growing the bar.");
        Check(DockMetrics.ResolveHeight(DockHeightMode.Percent, 0, 5, 1000, 40) == 50
            && DockMetrics.ResolveHeight(DockHeightMode.Pixels, 20, 0, 1000, 40) == 40,
            "Height resolution must honor percentages and clamp pixels below the content minimum.");

        // Drop coordinates count visible elements, not hidden entries in the saved order.
        var dragItems = new[] { "app:A", "app:B", "widget:Clock", "widget:Audio" };
        Check(DockItemOrdering.MoveVisibleItem(dragItems, dragItems, "widget:Clock", 2).SequenceEqual(dragItems),
            "Dropping an item in its own slot must not move it.");
        Check(DockItemOrdering.MoveVisibleItem(dragItems, dragItems, "widget:Clock", 1)
                .SequenceEqual(["app:A", "widget:Clock", "app:B", "widget:Audio"]),
            "Moving one slot left must not jump to the start.");
        Check(DockItemOrdering.MoveVisibleItem(dragItems, dragItems, "app:B", 3)
                .SequenceEqual(["app:A", "widget:Clock", "app:B", "widget:Audio"]),
            "Moving one slot right must account for removal of the source.");
        Check(DockItemOrdering.MoveVisibleItem(dragItems, dragItems, "app:B", 4)
                .SequenceEqual(["app:A", "widget:Clock", "widget:Audio", "app:B"]),
            "Dropping at the far right must reach the last visible position.");

        var stored = new[] { "app:A", "app:closed", "app:B", "widget:disabled", "widget:Clock" };
        var shown = new[] { "app:A", "app:B", "widget:Clock" };
        var reordered = DockItemOrdering.MoveVisibleItem(stored, shown, "widget:Clock", 1);
        Check(reordered.SequenceEqual(["app:A", "app:closed", "widget:Clock", "widget:disabled", "app:B"]),
            "Inactive saved items must not shift the visible insertion point.");
        Check(reordered.Where(token => shown.Contains(token)).SequenceEqual(["app:A", "widget:Clock", "app:B"]),
            "The displayed order after dropping must match the pointer position.");
        Check(DockItemOrdering.MoveVisibleItem(stored, shown, "widget:Clock", 0)
                .Where(token => shown.Contains(token)).SequenceEqual(["widget:Clock", "app:A", "app:B"]),
            "Moving to the first visible slot must still work when inactive items are saved.");
        Check(DockItemOrdering.MoveVisibleItem(stored, shown, "app:B", 3)
                .Where(token => shown.Contains(token)).SequenceEqual(["app:A", "widget:Clock", "app:B"]),
            "Moving right past hidden items must remain an adjacent move.");
        Check(DockItemOrdering.MoveVisibleItem(stored, shown, "app:unknown", 0).SequenceEqual(stored),
            "An item not currently visible must not disturb the saved order.");
        Check(DockItemOrdering.MoveVisibleItem(dragItems, dragItems, "APP:B", 0)[0] == "app:B",
            "Dock item identifiers must be matched without case sensitivity.");

        var minimum = (double)app.FindResource("DockMinimumShelfHeight");
        var headroom = (double)app.FindResource("DockHeadroom");
        foreach (var dpi in new uint[] { 96, 120, 144, 192 })
            foreach (var nativeHeight in new[] { 0d, 48d, 100d })
            {
                var metrics = DockLayoutMetrics.Calculate(minimum, nativeHeight, headroom, dpi);
                var nativePixels = (int)Math.Ceiling(nativeHeight * dpi / 96d);
                Check(metrics.ShelfPixels * 96d / dpi >= Math.Max(minimum, nativeHeight),
                    "The reserved shelf must fit the dock and native taskbar at every supported scale.");
                Check(metrics.WindowPixels - metrics.ShelfPixels == metrics.HeadroomPixels,
                    "Hover headroom must enlarge the window without enlarging the desktop reservation.");
                Check(nativePixels + metrics.AdditionalReservation(nativePixels, false) == metrics.ShelfPixels,
                    "Existing native space plus the extra AppBar must reserve exactly one shelf.");
                Check(metrics.AdditionalReservation(0, false) == metrics.ShelfPixels,
                    "When Explorer reserves no strip, OpenTaskbar must reserve the complete shelf.");
                Check(metrics.AdditionalReservation(metrics.ShelfPixels + 20, false) == 0,
                    "A larger existing reservation must not receive another shelf.");
                Check(metrics.AdditionalReservation(nativePixels, true) == 0,
                    "Auto-hide must release the additional desktop reservation.");
            }

        // Secondary docks must appear even when taskbar replacement releases Explorer's strip.
        Check(SecondaryDockWindow.NeedsOwnShelf(0, 96), "A released strip must make the secondary dock reserve a shelf.");
        Check(!SecondaryDockWindow.NeedsOwnShelf(66, 96), "An existing taskbar strip must be reused as is.");
        Check(SecondaryDockWindow.NeedsOwnShelf(66, 192), "A strip must be measured in DPI-aware pixels.");

        // A secondary dock's own app-bar shelf contributes to the monitor work-area strip.
        // Seeing that space again must not cause the dock to release and re-register its shelf.
        foreach (var dpi in new uint[] { 96, 120, 144, 192 })
        {
            var shelf = DockLayoutMetrics.Calculate(DockMetrics.ShelfHeight, 0, 0, dpi).ShelfPixels;
            Check(SecondaryDockWindow.MissingShelfPixels(shelf, shelf, dpi) == shelf,
                "An existing self-owned shelf must not be mistaken for Explorer's taskbar reservation.");
            Check(SecondaryDockWindow.MissingShelfPixels(shelf + 12, shelf, dpi) == Math.Max(0, shelf - 12),
                "Only the external portion of a mixed work-area strip must reduce our reservation.");
            Check(SecondaryDockWindow.MissingShelfPixels(shelf + 20, 0, dpi) == 0,
                "An adequate external taskbar strip must make our shelf unnecessary.");
            Check(SecondaryDockWindow.MissingShelfPixels(shelf * 2, shelf, dpi) == 0,
                "A returning native taskbar must release our shelf.");
        }

        // Primary replacement session shelf reservation when returning from side dock (Explorer strip released)
        foreach (var dpi in new uint[] { 96, 120, 144, 192 })
            foreach (var size in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
            {
                var profile = DockMetrics.For(size);
                var metrics = DockLayoutMetrics.Calculate(profile.Shelf, 0, DockMetrics.Headroom, dpi);
                Check(metrics.AdditionalReservation(0, false) == metrics.ShelfPixels,
                    $"Returning from side dock with released strip must reserve the full shelf for {size}@{dpi}dpi.");
                var nativePixels = (int)Math.Ceiling(48d * dpi / 96d);
                var expectedExtra = Math.Max(0, metrics.ShelfPixels - nativePixels);
                Check(metrics.AdditionalReservation(nativePixels, false) == expectedExtra,
                    $"Existing native taskbar strip must reduce the primary dock reservation for {size}@{dpi}dpi.");
            }

        Check((SecondaryDockWindow.PositionFlags(false) & 0x0040u) == 0,
            "Positioning a fullscreen-hidden secondary dock must not force it visible.");
        Check((SecondaryDockWindow.PositionFlags(true) & 0x0040u) != 0,
            "Positioning a visible secondary dock must keep it shown.");

        foreach (var width in new[] { 140d, 280d, 500d })
        {
            var iconSize = (double)app.FindResource("DockIconSize");
            var cellSize = (double)app.FindResource("DockCellSize");
            var icon = new Border
            {
                Width = iconSize,
                Height = iconSize,
                Background = Brushes.Cyan,
                RenderTransformOrigin = new Point(0.5, 0.95),
                RenderTransform = new TransformGroup
                {
                    Children = { new ScaleTransform(cellSize / iconSize, cellSize / iconSize), new TranslateTransform(0, -12) }
                }
            };
            var cell = new Grid { Width = cellSize, Height = cellSize };
            cell.Children.Add(icon);
            var content = new StackPanel { Orientation = Orientation.Horizontal, Width = 800, Height = cellSize, Background = Brushes.Magenta };
            content.Children.Add(cell);
            var viewer = new DockScrollViewer
            {
                Style = (Style)app.FindResource("DockScrollViewerStyle"),
                HoverHeadroom = headroom,
                ContentInset = (Thickness)app.FindResource("DockScrollContentInset"),
                Content = content
            };
            var shelf = new Border
            {
                Padding = (Thickness)app.FindResource("DockPadding"),
                BorderThickness = new Thickness(1),
                Margin = new Thickness(0, 0, 0, 6),
                Child = viewer
            };
            var root = new Grid { Width = width, Height = minimum + headroom };
            root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(headroom) });
            root.RowDefinitions.Add(new RowDefinition());
            Grid.SetRow(shelf, 1);
            root.Children.Add(shelf);

            void Layout()
            {
                root.Measure(new Size(width, minimum + headroom));
                root.Arrange(new Rect(0, 0, width, minimum + headroom));
                root.UpdateLayout();
            }
            Color PixelAt(Point point, double scale)
            {
                var bitmap = new RenderTargetBitmap((int)Math.Ceiling(width * scale),
                    (int)Math.Ceiling(root.Height * scale), 96 * scale, 96 * scale, PixelFormats.Pbgra32);
                bitmap.Render(root);
                var pixel = new byte[4];
                bitmap.CopyPixels(new Int32Rect((int)(point.X * scale), (int)(point.Y * scale), 1, 1), pixel, 4, 0);
                return Color.FromArgb(pixel[3], pixel[2], pixel[1], pixel[0]);
            }

            Layout();
            var bounds = icon.TransformToAncestor(root).TransformBounds(new Rect(0, 0, iconSize, iconSize));
            var sample = new Point(bounds.X + bounds.Width / 2, Math.Ceiling(bounds.Top) + 2);
            Check(sample.Y >= 0 && sample.Y < headroom,
                "The fixture must exercise magnification and bounce above the visible shelf.");
            foreach (var scale in new[] { 1d, 2d })
                Check(PixelAt(sample, scale) == Colors.Cyan,
                    "The actual dock scroll template must paint the top of a magnified, bouncing icon.");
            var right = viewer.TranslatePoint(new Point(viewer.ActualWidth + 2, viewer.ActualHeight - 12), root);
            Check(PixelAt(right, 1) != Colors.Magenta, "Horizontal overflow must remain clipped beside the resize gutter.");

            viewer.HoverHeadroom = 0;
            Layout();
            var clippedBounds = icon.TransformToAncestor(root).TransformBounds(new Rect(0, 0, iconSize, iconSize));
            var clippedSample = new Point(clippedBounds.X + clippedBounds.Width / 2, Math.Ceiling(clippedBounds.Top) + 2);
            Check(PixelAt(clippedSample, 1) != Colors.Cyan,
                "The regression fixture must reproduce the old clipping when internal headroom is removed.");
            viewer.HoverHeadroom = headroom;
            Layout();
            viewer.ScrollToHorizontalOffset(80);
            Layout();
            Check(viewer.HorizontalOffset == 80 && viewer.VerticalOffset == 0,
                "Hover headroom must preserve native horizontal scrolling without vertical scrolling.");
        }
        var cellWidth = (double)app.FindResource("DockCellSize");
        var iconWidth = (double)app.FindResource("DockIconSize");
        var padding = (Thickness)app.FindResource("DockPadding");
        var stroke = (Thickness)app.FindResource("ControlStroke");
        var inset = (Thickness)app.FindResource("DockScrollContentInset");
        var chrome = padding.Left + padding.Right + stroke.Left + stroke.Right + inset.Left + inset.Right;
        var oneCellWidth = cellWidth + chrome;
        foreach (var width in new[] { oneCellWidth, cellWidth * 2 + chrome, 600d, 1000d })
        {
            var icons = Enumerable.Range(0, 10).Select(index => new Border
            {
                Width = iconWidth,
                Height = iconWidth,
                Background = Brushes.Cyan
            }).ToArray();
            var buttons = icons.Select((icon, index) => new Button
            {
                Style = (Style)app.FindResource("DockButton"),
                Content = icon,
                DataContext = new DockApp($"Test app {index}", $"test-{index}.exe")
            }).ToArray();
            var content = new StackPanel { Orientation = Orientation.Horizontal };
            foreach (var button in buttons) content.Children.Add(button);
            var viewer = new DockScrollViewer
            {
                Style = (Style)app.FindResource("DockScrollViewerStyle"),
                HoverHeadroom = headroom,
                ContentInset = inset,
                Content = content
            };
            var frame = new Border { Padding = padding, BorderThickness = stroke, Background = Brushes.Black, Child = viewer };
            var left = new Thumb { Style = (Style)app.FindResource("DockResizeThumb"), HorizontalAlignment = HorizontalAlignment.Left };
            var right = new Thumb { Style = (Style)app.FindResource("DockResizeThumb"), HorizontalAlignment = HorizontalAlignment.Right };
            var surface = new Grid { Width = width, Height = minimum };
            surface.Children.Add(frame);
            surface.Children.Add(left);
            surface.Children.Add(right);
            void Layout()
            {
                surface.Measure(new Size(width, minimum));
                surface.Arrange(new Rect(0, 0, width, minimum));
                surface.UpdateLayout();
            }
            bool IsWithin(DependencyObject? hit, DependencyObject target)
            {
                while (hit is not null)
                {
                    if (ReferenceEquals(hit, target)) return true;
                    hit = VisualTreeHelper.GetParent(hit);
                }
                return false;
            }
            Layout();
            Check(buttons.All(button => button.ActualWidth == cellWidth && button.ActualHeight == cellWidth)
                && icons.All(icon => icon.ActualWidth == iconWidth && icon.ActualHeight == iconWidth),
                "Narrowing the dock must preserve each application cell and icon size.");
            Check(viewer.HorizontalScrollBarVisibility == ScrollBarVisibility.Hidden
                && viewer.Template.FindName("PART_HorizontalScrollBar", viewer) is null,
                "Overflow must remain scrollable without a visible scrollbar.");
            Check(IsWithin(VisualTreeHelper.HitTest(surface, new Point(0.5, minimum / 2))?.VisualHit, left)
                && IsWithin(VisualTreeHelper.HitTest(surface, new Point(width - 0.5, minimum / 2))?.VisualHit, right),
                "Resize hit targets must cover the actual outside borders rather than the content padding.");
            left.ApplyTemplate();
            var grip = left.Template.FindName("GripIndicator", left) as FrameworkElement;
            Check(grip is not null, "The resize thumb must provide a visual grip indicator.");
            if (width == oneCellWidth)
                Check(viewer.ViewportWidth == cellWidth, "The minimum dock width must expose exactly one complete application cell.");
            var widget = new ContentControl { Content = new OpenTaskbar.Models.DockWidget("Clock", "Clock") { Value = "14:32" } };
            var previousExtent = viewer.ExtentWidth;
            content.Children.Add(widget);
            Layout();
            Check(viewer.ExtentWidth > previousExtent
                && buttons.All(button => button.ActualWidth == cellWidth)
                && icons.All(icon => icon.ActualWidth == iconWidth),
                "Adding widgets must extend scrollable content without resizing application icons.");
            if (viewer.ScrollableWidth > 0)
            {
                Check(viewer.ScrollByWheel(-Mouse.MouseWheelDeltaForOneLine, ModifierKeys.Shift), "Shift+wheel must be handled for overflowing content.");
                Layout();
                Check(viewer.HorizontalOffset == cellWidth, "One wheel notch must move one application cell.");
                Check(viewer.ScrollByWheel(-Mouse.MouseWheelDeltaForOneLine / 4, ModifierKeys.Control), "Ctrl+wheel must support high-resolution deltas.");
                Layout();
                Check(viewer.HorizontalOffset == Math.Min(cellWidth * 1.25, viewer.ScrollableWidth), "High-resolution wheel deltas must preserve fractional cell movement.");
                Check(viewer.ScrollByWheel(-120, ModifierKeys.None) && !viewer.ScrollByWheel(0, ModifierKeys.None),
                    "Plain wheel must scroll horizontal-only overflow; zero input must do nothing.");
                viewer.ScrollToHorizontalOffset(0);
                Layout();
                var prevIndicator = viewer.Template.FindName("PART_PreviousButton", viewer) as Button;
                var nextIndicator = viewer.Template.FindName("PART_NextButton", viewer) as Button;
                Check(prevIndicator is not null && nextIndicator is not null, "DockScrollViewer must provide Previous and Next chevron indicator buttons.");
                if (prevIndicator is null || nextIndicator is null) throw new InvalidOperationException("Indicator buttons not found.");
                Check(prevIndicator.Visibility == Visibility.Collapsed && nextIndicator.Visibility == Visibility.Visible,
                    "At offset 0 with overflow, previous indicator must be collapsed and next indicator visible.");
                viewer.ScrollByWheel(int.MinValue, ModifierKeys.Control | ModifierKeys.Shift);
                Layout();
                Check(viewer.HorizontalOffset == viewer.ScrollableWidth && viewer.VerticalOffset == 0,
                    "Forward scrolling must stop at the content end without vertical movement.");
                Check(prevIndicator.Visibility == Visibility.Visible && nextIndicator.Visibility == Visibility.Collapsed,
                    "At maximum offset, previous indicator must be visible and next indicator collapsed.");
                viewer.ScrollToHorizontalOffset(cellWidth);
                Layout();
                Check(prevIndicator.Visibility == Visibility.Visible && nextIndicator.Visibility == Visibility.Visible,
                    "When scrolled in between both ends, both indicators must be visible.");
                viewer.ScrollByWheel(int.MaxValue, ModifierKeys.Shift);
                Layout();
                Check(viewer.HorizontalOffset == 0, "Backward scrolling must stop at the content start.");

                // A deferred resize after scrolling must be able to reset the strip without leaving
                // an in-flight smooth-scroll target that would pull it back to the old offset.
                viewer.ScrollToHorizontalOffset(cellWidth);
                Layout();
                viewer.ScrollToHorizontalOffset(cellWidth);
                Layout();
                viewer.ResetHorizontalScroll();
                Layout();
                Check(viewer.HorizontalOffset == 0,
                    "ResetHorizontalScroll must return a scrolled strip to the start.");

                // External edge overlays (mounted in DockSurface beside the resize thumbs) must take over
                // indicator visibility and stay bounded inside the shelf border without HoverHeadroom offset.
                var extPrev = new Button { Style = (Style)app.FindResource("DockScrollIndicatorButton") };
                var extNext = new Button { Style = (Style)app.FindResource("DockScrollIndicatorButton") };
                viewer.ExternalIndicatorCornerRadius = 19;
                viewer.SetExternalIndicators(extPrev, extNext);
                Layout();
                Check(prevIndicator.Visibility == Visibility.Collapsed && nextIndicator.Visibility == Visibility.Collapsed,
                    "Internal template scroll buttons must stay collapsed when external dock-edge overlays are registered.");
                Check(extPrev.Visibility == Visibility.Collapsed && extNext.Visibility == Visibility.Visible,
                    "At offset 0 with external overlays, only the right dock-edge overlay must be visible.");
                Check(extPrev.Margin == new Thickness(1, 1, 0, 1) && extNext.Margin == new Thickness(0, 1, 1, 1),
                    "External dock-edge overlays must inset by 1 DIP inside DockFrame without HoverHeadroom top offset.");
                Check(extPrev.Tag is CornerRadius prevCorner && prevCorner == new CornerRadius(19, 0, 0, 19)
                    && extNext.Tag is CornerRadius nextCorner && nextCorner == new CornerRadius(0, 19, 19, 0),
                    "External dock-edge overlays must match DockFrame inner corner radii.");
                Check(extPrev.Background is not null && extNext.Background is not null,
                    "External dock-edge overlays must carry the directional edge scrim brushes.");
                viewer.ScrollToHorizontalOffset(cellWidth);
                Layout();
                Check(extPrev.Visibility == Visibility.Visible && extNext.Visibility == Visibility.Visible,
                    "Mid-scroll with external overlays must show both left and right dock-edge indicators.");
                viewer.ScrollToHorizontalOffset(0);
                viewer.SetExternalIndicators(null, null);
                Layout();
            }
            else Check(!viewer.ScrollByWheel(-120, ModifierKeys.Control), "A dock that already fits must not consume modifier wheel scrolling.");
        }

        // The middle button is reserved for panning the dock. It must capture even when nothing overflows:
        // otherwise a middle-click over an app icon reaches DockApp_MouseUp and launches a new instance.
        {
            var viewer = new DockScrollViewer
            {
                Width = 400,
                Height = 80,
                Content = new Border { Width = 120, Height = 40 }
            };
            var window = new Window
            {
                Left = -20000,
                Top = -20000,
                Width = 400,
                Height = 80,
                ShowActivated = false,
                ShowInTaskbar = false,
                Content = viewer
            };
            try
            {
                window.Show();
                window.UpdateLayout();
                Check(viewer.ScrollableWidth <= 0.5, "The pan test dock must not overflow.");
                viewer.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Middle)
                {
                    RoutedEvent = UIElement.PreviewMouseDownEvent,
                    Source = viewer
                });
                Check(viewer.IsPanning, "Middle button must start panning even when the dock has no overflow.");
                viewer.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Middle)
                {
                    RoutedEvent = UIElement.PreviewMouseUpEvent,
                    Source = viewer
                });
                Check(!viewer.IsPanning, "Releasing the middle button must end panning.");
            }
            finally
            {
                window.Close();
            }
        }

        {
            var apps = new ObservableCollection<DockApp>(Enumerable.Range(0, 4)
                .Select(index => new DockApp($"Running app {index}", $"running-{index}.exe")));
            var appButton = new FrameworkElementFactory(typeof(Button));
            appButton.SetValue(FrameworkElement.StyleProperty, app.FindResource("DockButton"));
            var appPanel = new FrameworkElementFactory(typeof(StackPanel));
            appPanel.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal);
            var dockContents = new StackPanel { Orientation = Orientation.Horizontal };
            dockContents.Children.Add(new Button { Style = (Style)app.FindResource("DockButton") });
            dockContents.Children.Add(new ItemsControl
            {
                ItemsSource = apps,
                ItemTemplate = new DataTemplate { VisualTree = appButton },
                ItemsPanel = new ItemsPanelTemplate(appPanel)
            });
            var widgetIsland = new Border { Width = 120, Height = iconWidth, Margin = new Thickness(4, 0, 0, 0) };
            var widgetsWidth = widgetIsland.Width + widgetIsland.Margin.Left + widgetIsland.Margin.Right;
            var dockLayoutRoot = new Grid();
            dockLayoutRoot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            dockLayoutRoot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            Grid.SetColumn(widgetIsland, 1);
            dockLayoutRoot.Children.Add(dockContents);
            dockLayoutRoot.Children.Add(widgetIsland);
            var dockViewer = new DockScrollViewer
            {
                Style = (Style)app.FindResource("DockScrollViewerStyle"),
                HoverHeadroom = headroom,
                ContentInset = inset,
                Content = dockLayoutRoot
            };
            var dockSurface = new Border
            {
                Padding = padding,
                BorderThickness = stroke,
                HorizontalAlignment = HorizontalAlignment.Left,
                Child = dockViewer
            };
            var dockRoot = new Grid { Width = 1000, Height = minimum };
            dockRoot.Children.Add(dockSurface);
            void LayoutDock()
            {
                dockRoot.Measure(new Size(1000, minimum));
                dockRoot.Arrange(new Rect(0, 0, 1000, minimum));
                dockRoot.UpdateLayout();
            }
            // Mirrors the main window, which sizes the dock synchronously after its app collections change.
            void FitDock() => dockSurface.Width = Math.Ceiling(dockViewer.MeasureContentWidth(dockContents) + widgetsWidth + chrome);
            LayoutDock();
            FitDock();
            var initialWidth = dockSurface.Width;
            LayoutDock();
            Check(dockViewer.ScrollableWidth == 0 && dockViewer.ViewportWidth == dockViewer.ExtentWidth,
                "The fitted dock must show its applications and widgets without empty space.");
            apps.RemoveAt(1);
            FitDock();
            Check(dockSurface.Width == initialWidth - cellWidth,
                "Removing an application must shrink the dock immediately instead of leaving an empty cell.");
            LayoutDock();
            Check(dockViewer.ScrollableWidth == 0 && dockViewer.ViewportWidth == dockViewer.ExtentWidth,
                "The shrunken dock must show all remaining applications without empty space.");
            apps.Add(new DockApp("Opened app", "opened.exe"));
            FitDock();
            Check(dockSurface.Width == initialWidth, "Opening an application must grow the dock immediately.");
            var widgetBefore = widgetsWidth;
            widgetsWidth -= 40;
            FitDock();
            Check(dockSurface.Width == initialWidth - 40, "Removing a widget must shrink the dock width immediately.");
            widgetsWidth = widgetBefore;
            FitDock();
            Check(dockSurface.Width == initialWidth, "Restoring widget width must resize dock width immediately.");
        }

        {
            var grid = new AdaptiveGrid { MinItemWidth = 230, Width = 500 };
            for (var i = 0; i < 4; i++) grid.Children.Add(new Border { Height = 100 });
            grid.Measure(new Size(500, double.PositiveInfinity));
            grid.Arrange(new Rect(0, 0, 500, 300));
            grid.UpdateLayout();
            Check(grid.Children[0].RenderSize.Width == 250, "AdaptiveGrid must divide 500 DIP width into 2 columns of 250 DIPs.");
            Check(grid.Children[1].RenderSize.Width == 250, "AdaptiveGrid items must stretch to fill their column.");
            Check(grid.Children[1].TranslatePoint(new Point(), grid).Y == 0, "Second item must be in row 0 at 500 DIP width.");
            Check(grid.Children[2].TranslatePoint(new Point(), grid).Y > 0, "Third item must wrap to row 1 at 500 DIP width.");

            grid.Width = 720;
            grid.Measure(new Size(720, double.PositiveInfinity));
            grid.Arrange(new Rect(0, 0, 720, 300));
            grid.UpdateLayout();
            Check(grid.Children[0].RenderSize.Width == 240, "AdaptiveGrid must adapt to 3 columns of 240 DIPs at 720 DIP width.");
            Check(grid.Children[2].TranslatePoint(new Point(), grid).Y == 0, "Third item must fit on row 0 in 3-column mode.");
        }

        {
            var tokens = new List<string> { "start", "app:pin1.exe", "widget:clock", "widget:media" };
            var newToken = "app:running1.exe";
            var lastAppIndex = tokens.FindLastIndex(t => t.StartsWith("app:", StringComparison.OrdinalIgnoreCase));
            if (lastAppIndex >= 0) tokens.Insert(lastAppIndex + 1, newToken);
            else tokens.Add(newToken);
            Check(tokens.IndexOf("app:running1.exe") == 2, "Newly opened app must be inserted right after the existing app and before widgets.");
            Check(tokens.IndexOf("widget:clock") == 3 && tokens.IndexOf("widget:media") == 4, "Widgets must remain on the right of the newly opened app.");
        }

        var testWindow = new Window { ShowActivated = false, ShowInTaskbar = false, Width = 10, Height = 10, WindowStyle = WindowStyle.None };
        var helper = new System.Windows.Interop.WindowInteropHelper(testWindow);
        helper.EnsureHandle();
        Check(MainWindow.ExcludeFromTaskSwitcher(helper.Handle), "Task switcher exclusion must successfully apply to windows.");
        var exStyle = MainWindow.GetWindowLongPtr(helper.Handle, -20);
        Check((exStyle & 0x00000080) != 0, "Task switcher exclusion must enforce WS_EX_TOOLWINDOW.");
        Check((exStyle & 0x00040000) == 0, "Task switcher exclusion must clear WS_EX_APPWINDOW.");
        testWindow.Close();

        var targetSize = new Size(48, 48);
        var popupSize = new Size(120, 26);
        DockFlyout.Edge = DockEdge.Bottom;
        var bottomPlacements = DockMetrics.PlaceDockTooltip(popupSize, targetSize, new Point(0, 0));
        Check(bottomPlacements.Length == 2, "Bottom dock tooltip must provide primary and fallback placements.");
        Check(bottomPlacements[0].Point.X == (targetSize.Width - popupSize.Width) / 2, "Bottom dock tooltip primary placement must be horizontally centered.");
        Check(bottomPlacements[0].Point.Y == -popupSize.Height - DockMetrics.FlyoutGap, "Bottom dock tooltip primary placement must sit the current flyout gap above the button.");
        Check(bottomPlacements[1].Point.X == (targetSize.Width - popupSize.Width) / 2, "Bottom dock tooltip fallback placement must be horizontally centered.");
        Check(bottomPlacements[1].Point.Y == targetSize.Height + DockMetrics.FlyoutGap, "Bottom dock tooltip fallback placement must sit the current flyout gap below the button.");

        DockFlyout.Edge = DockEdge.Left;
        var leftPlacements = DockMetrics.PlaceDockTooltip(popupSize, targetSize, new Point(0, 0));
        Check(leftPlacements[0].Point.X == targetSize.Width + DockMetrics.FlyoutGap, "Left edge primary placement must be to the right of the dock.");
        Check(leftPlacements[0].Point.Y == (targetSize.Height - popupSize.Height) / 2, "Left edge primary placement must be vertically centered.");

        DockFlyout.Edge = DockEdge.Right;
        var rightPlacements = DockMetrics.PlaceDockTooltip(popupSize, targetSize, new Point(0, 0));
        Check(rightPlacements[0].Point.X == -popupSize.Width - DockMetrics.FlyoutGap, "Right edge primary placement must be to the left of the dock.");
        Check(rightPlacements[0].Point.Y == (targetSize.Height - popupSize.Height) / 2, "Right edge primary placement must be vertically centered.");
        DockFlyout.Edge = DockEdge.Bottom;

        foreach (var size in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
            foreach (var scale in new[] { 0.75, 1.0, 1.25, 1.5 })
            {
                var profile = DockMetrics.For(size, scale);
                var flyoutGap = DockMetrics.ComputeFlyoutGap(profile);
                Check(flyoutGap >= DockMetrics.FlyoutGapMin && flyoutGap <= DockMetrics.FlyoutGapMax,
                    $"Flyout gap for {size}@{scale} must stay within {DockMetrics.FlyoutGapMin}–{DockMetrics.FlyoutGapMax} px of the dock.");
                Check(Math.Round(DockMetrics.WidgetDesignHeight * profile.WidgetScale) + 2 <= profile.Cell,
                    $"Widget strip scale for {size}@{scale} must fit inside the dock cell height.");
                app.Resources["DockCellSize"] = profile.Cell;
                app.Resources["DockCellHeight"] = profile.Cell;
                app.Resources["DockIconSize"] = profile.Icon;
                var button = new Button { Style = (Style)app.FindResource("DockButton"), DataContext = new DockApp("App", "app.exe") { WindowCount = 1, IsForeground = true } };
                button.Measure(new Size(profile.Cell, profile.Cell));
                button.Arrange(new Rect(0, 0, profile.Cell, profile.Cell));
                button.UpdateLayout();
                Check(button.ActualWidth == profile.Cell && button.ActualHeight == profile.Cell,
                    $"DockButton must dynamically resize to {profile.Cell} DIP for {size}@{scale}.");
            }
        // Crisp compact widgets: cards are laid out at their real height (34–46 DIP) via the dynamic WidgetHeight
        // resource, and the default 100% profiles never get a fractional (blurry) LayoutTransform.
        foreach (var size in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
        {
            var profile = DockMetrics.For(size, 1.0);
            Check(profile.WidgetLayoutScale == 1.0,
                $"{size}@100% widgets must render unscaled (WidgetLayoutScale=1.0), got {profile.WidgetLayoutScale}.");
            Check(profile.WidgetCardHeight >= DockMetrics.WidgetMinCardHeight && profile.WidgetCardHeight <= DockMetrics.WidgetDesignHeight
                    && profile.WidgetCardHeight + 2 <= profile.EffectiveCellHeight,
                $"{size}@100% widget card height {profile.WidgetCardHeight} must fit the cell at its real size.");
        }
        var compact80 = DockMetrics.For(DockSize.Compact, 0.8);
        Check(compact80.WidgetCardHeight == DockMetrics.WidgetMinCardHeight,
            $"Compact@80% widget card height must be {DockMetrics.WidgetMinCardHeight}, got {compact80.WidgetCardHeight}.");
        Check(compact80.WidgetLayoutScale > 0.80,
            $"Compact@80% widget residual scale must stay above 0.80 (was 0.61 from 46 DIP), got {compact80.WidgetLayoutScale:0.####}.");
        foreach (var size in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
        foreach (var scale in new[] { 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5 })
        {
            var profile = DockMetrics.For(size, scale);
            Check(Math.Round(profile.WidgetCardHeight * profile.WidgetLayoutScale) + 2 <= profile.EffectiveCellHeight,
                $"Scaled widget card for {size}@{scale} ({profile.WidgetCardHeight}×{profile.WidgetLayoutScale:0.###}) must fit the {profile.EffectiveCellHeight} DIP cell.");
        }
        var originalWidgetHeight = app.Resources["WidgetHeight"];
        var widgetHostWindow = new Window();
        try
        {
            app.Resources["WidgetHeight"] = 34.0;
            var card = new Border { Style = (Style)app.FindResource("WidgetCardBorder") };
            var cardButton = new Button { Style = (Style)app.FindResource("WidgetCardButton") };
            var hostPanel = new StackPanel();
            hostPanel.Children.Add(card);
            hostPanel.Children.Add(cardButton);
            widgetHostWindow.Content = hostPanel;
            Check(card.Height == 34.0 && cardButton.Height == 34.0,
                $"WidgetCardBorder/WidgetCardButton must follow dynamic WidgetHeight=34 (got {card.Height}/{cardButton.Height}).");
            app.Resources["WidgetHeight"] = DockMetrics.WidgetDesignHeight;
            Check(card.Height == DockMetrics.WidgetDesignHeight && cardButton.Height == DockMetrics.WidgetDesignHeight,
                $"WidgetCardBorder/WidgetCardButton must update live when WidgetHeight changes back to 46 (got {card.Height}/{cardButton.Height}).");
        }
        finally
        {
            widgetHostWindow.Content = null;
            widgetHostWindow.Close();
            app.Resources["WidgetHeight"] = originalWidgetHeight ?? DockMetrics.WidgetDesignHeight;
        }
        var unscaledHost = new StackPanel();
        MainWindow.ApplyWidgetScale(unscaledHost, 1.0);
        Check(unscaledHost.LayoutTransform == Transform.Identity
                && TextOptions.GetTextFormattingMode(unscaledHost) == TextFormattingMode.Display,
            "Unscaled widget strip must use no transform and pixel-snapped Display text.");
        var scaledHost = new StackPanel();
        MainWindow.ApplyWidgetScale(scaledHost, compact80.WidgetLayoutScale);
        Check(scaledHost.LayoutTransform is ScaleTransform { ScaleX: var sx } && Math.Abs(sx - compact80.WidgetLayoutScale) < 0.0001
                && TextOptions.GetTextFormattingMode(scaledHost) == TextFormattingMode.Ideal
                && TextOptions.GetTextRenderingMode(scaledHost) == TextRenderingMode.Grayscale
                && RenderOptions.GetBitmapScalingMode(scaledHost) == BitmapScalingMode.HighQuality,
            "Scaled widget strip must switch to Ideal/Grayscale vector text with high-quality bitmap scaling.");
        // The scale slider must change the height Windows reserves for the taskbar, including the default
        // Compact size where smaller scales produce a shelf below Explorer's native 48 DIP strip.
        var compactReservations = new[] { 0.75, 0.9, 1.0, 1.25, 1.5 }
            .Select(scale => DockLayoutMetrics.Calculate(DockMetrics.For(DockSize.Compact, scale).Shelf, 0, 0, 96).ShelfPixels)
            .ToArray();
        for (var index = 1; index < compactReservations.Length; index++)
        {
            Check(compactReservations[index] > compactReservations[index - 1],
                "Raising the taskbar scale must grow the reserved shelf at every step.");
        }

        // Secondary docks must use the same scaled shelf rather than the unscaled constant.
        var compactSmall = DockMetrics.For(DockSize.Compact, 0.75).Shelf;
        var compactFull = DockMetrics.For(DockSize.Compact, 1.0).Shelf;
        Check(SecondaryDockWindow.NeedsOwnShelf(0, 96, compactSmall),
            "A scaled Compact shelf must still reserve a secondary-dock shelf when Explorer releases its strip.");
        Check(SecondaryDockWindow.MissingShelfPixels(0, 0, 96, compactSmall)
                < SecondaryDockWindow.MissingShelfPixels(0, 0, 96, compactFull),
            "The secondary dock shelf must follow the taskbar scale.");

        app.Resources["DockCellSize"] = DockMetrics.CellSize;
        app.Resources["DockCellHeight"] = DockMetrics.CellSize;
        app.Resources["DockIconSize"] = DockMetrics.IconSize;

        // Verify that narrowing the dock with both applications and WidgetIsland inside DockScrollViewer keeps the first application visible.
        {
            var firstButton = new Button
            {
                Style = (Style)app.FindResource("DockButton"),
                Content = new Border { Width = iconWidth, Height = iconWidth, Background = Brushes.Cyan },
                DataContext = new DockApp("First App", "first.exe")
            };
            var secondButton = new Button
            {
                Style = (Style)app.FindResource("DockButton"),
                Content = new Border { Width = iconWidth, Height = iconWidth, Background = Brushes.Cyan },
                DataContext = new DockApp("Second App", "second.exe")
            };
            var dockContents = new StackPanel { Orientation = Orientation.Horizontal };
            dockContents.Children.Add(firstButton);
            dockContents.Children.Add(secondButton);
            var widgetIsland = new Border { Width = 600, Height = 38, Margin = new Thickness(6, 0, 0, 0), Background = Brushes.Orange };
            var layoutRoot = new Grid();
            layoutRoot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            layoutRoot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            Grid.SetColumn(dockContents, 0);
            Grid.SetColumn(widgetIsland, 1);
            layoutRoot.Children.Add(dockContents);
            layoutRoot.Children.Add(widgetIsland);
            var viewer = new DockScrollViewer
            {
                Style = (Style)app.FindResource("DockScrollViewerStyle"),
                HoverHeadroom = headroom,
                ContentInset = inset,
                Content = layoutRoot
            };
            var frame = new Border { Padding = padding, BorderThickness = stroke, Child = viewer };
            var surface = new Grid { Width = oneCellWidth, Height = minimum };
            surface.Children.Add(frame);
            surface.Measure(new Size(oneCellWidth, minimum));
            surface.Arrange(new Rect(0, 0, oneCellWidth, minimum));
            surface.UpdateLayout();
            var firstBounds = firstButton.TransformToAncestor(surface).TransformBounds(new Rect(0, 0, firstButton.ActualWidth, firstButton.ActualHeight));
            Check(firstBounds.Left >= 0 && firstBounds.Right <= oneCellWidth + 0.5 && firstButton.ActualWidth == cellWidth,
                "Narrowing the dock to one cell must keep the first application visible instead of hiding it behind widgets.");
            Check(viewer.ScrollableWidth > 500, "Overflowing widgets must remain reachable by scrolling to the right.");
        }

        // Verify dynamic widget label localization
        var widgetService = new DockWidgetService(TimeProvider.System);
        var testWidgets = widgetService.CreateWidgets(DockWidgetService.SupportedIds.ToArray());
        UiText.Current.SetLanguage(AppLanguage.Turkish);
        DockWidgetService.Refresh(testWidgets, AppLanguage.Turkish);
        var clockTr = testWidgets.First(w => w.Id == DockWidgetService.ClockId);
        var stopwatchTr = testWidgets.First(w => w.Id == DockWidgetService.StopwatchId);
        Check(clockTr.Label == "Tarih ve saat", "Clock widget label must localize to Turkish.");
        Check(stopwatchTr.Label == "Kronometre", "Stopwatch widget label must localize to Turkish.");

        UiText.Current.SetLanguage(AppLanguage.English);
        DockWidgetService.Refresh(testWidgets, AppLanguage.English);
        Check(clockTr.Label == "Date and time", "Clock widget label must localize to English.");
        Check(stopwatchTr.Label == "Stopwatch", "Stopwatch widget label must localize to English.");

        // Verify TaskbarReplacement error keys localization
        Check(UiText.Get("ErrorTaskbarHide") == "Cannot hide the Windows taskbar.", "Taskbar hide error must localize to English.");
        Check(UiText.Get("ErrorTaskbarRestoreFailed").Contains("Cannot restore the Windows taskbar"), "Taskbar restore error must localize to English.");
        UiText.Current.SetLanguage(AppLanguage.Turkish);
        Check(UiText.Get("ErrorTaskbarHide") == "Windows görev çubuğu gizlenemedi.", "Taskbar hide error must localize to Turkish.");
        Check(UiText.Get("ErrorTaskbarRestoreFailed").Contains("Windows görev çubuğu geri yüklenemedi"), "Taskbar restore error must localize to Turkish.");

        // Verify optical glass materials and bevel resources
        var darkTheme = new ResourceDictionary { Source = new Uri("/OpenTaskbar;component/Design/Palettes/Dark.xaml", UriKind.Relative) };
        var lightTheme = new ResourceDictionary { Source = new Uri("/OpenTaskbar;component/Design/Palettes/Light.xaml", UriKind.Relative) };
        Check(darkTheme["DockBorderMaterial"] is LinearGradientBrush { GradientStops.Count: >= 4 }, "Dark theme must define multi-stop Fresnel rim light gradient.");
        Check(lightTheme["DockBorderMaterial"] is LinearGradientBrush { GradientStops.Count: >= 4 }, "Light theme must define multi-stop Fresnel rim light gradient.");
        Check(darkTheme["DockInnerBevelMaterial"] is LinearGradientBrush, "Dark theme must define optical inner bevel refraction line.");
        Check(lightTheme["DockInnerBevelMaterial"] is LinearGradientBrush, "Light theme must define optical inner bevel refraction line.");
        Check(darkTheme["DockHighlight"] is LinearGradientBrush, "Dark theme must define surface specular highlight.");
        Check(lightTheme["DockHighlight"] is LinearGradientBrush, "Light theme must define surface specular highlight.");
        Check(app.FindResource("StartDockButton") is Style, "StartDockButton tactile tile style must be defined.");

        // Verify WidgetCoordinator ordering, reordering and validation
        Check(WidgetCoordinator.DefaultWidgetOrder.Length == 21, "WidgetCoordinator must have 21 default widgets.");
        Check(WidgetCoordinator.GetLabelKey("Clock") == "WidgetClock", "Clock widget label key must match.");
        Check(WidgetCoordinator.GetLabelKey("Media") == "WidgetMedia", "Media widget label key must match.");
        Check(WidgetCoordinator.GetLabelKey("NetSpeed") == "WidgetNetSpeed", "NetSpeed widget label key must match.");
        Check(WidgetCoordinator.GetLabelKey("BluetoothBattery") == "WidgetBluetoothBattery", "BluetoothBattery widget label key must match.");
        Check(WidgetCoordinator.GetLabelKey("Battery") == "WidgetBattery", "Battery widget label key must match.");
        Check(WidgetCoordinator.GetLabelKey("Storage") == "WidgetStorage", "Storage widget label key must match.");
        Check(WidgetCoordinator.GetLabelKey("DayProgress") == "WidgetDayProgress", "DayProgress widget label key must match.");

        UiText.Current.SetLanguage(AppLanguage.Turkish);
        Check(UiText.Get("WidgetOrderHeading") == "Widget Sıralaması", "WidgetOrderHeading must localize to Turkish.");
        Check(UiText.Get("MoveUp") == "Yukarı taşı", "MoveUp must localize to Turkish.");
        UiText.Current.SetLanguage(AppLanguage.English);
        Check(UiText.Get("WidgetOrderHeading") == "Widget Order", "WidgetOrderHeading must localize to English.");
        Check(UiText.Get("MoveUp") == "Move up", "MoveUp must localize to English.");

        var testStrip = new StackPanel();
        using var coordinator = new WidgetCoordinator(testStrip, Dispatcher.CurrentDispatcher, _ => { });
        Check(coordinator.CurrentOrder.SequenceEqual(WidgetCoordinator.DefaultWidgetOrder), "Initial coordinator order must match defaults.");

        coordinator.ApplyOrder(["Clock", "ShowDesktop", "Focus", "Media"]);
        Check(coordinator.CurrentOrder[0] == "Clock" && coordinator.CurrentOrder[1] == "Focus" && coordinator.CurrentOrder[2] == "Media",
            "ApplyOrder must reorder widgets per specified order.");
        Check(testStrip.Children.Count == 21, "The retired ShowDesktop slot must be ignored when applying an old order.");
        var unspecified = coordinator.CurrentOrder.Skip(3).ToArray();
        coordinator.ApplyOrder(["Clock", "Focus", "Media"]);
        Check(coordinator.CurrentOrder.Skip(3).SequenceEqual(unspecified),
            "Reapplying a partial order must preserve all unspecified widget positions.");

        IReadOnlyList<string>? capturedOrder = null;
        coordinator.OrderChanged = order => capturedOrder = order;
        coordinator.ReorderWidgets(0, 1);
        Check(coordinator.CurrentOrder[0] == "Focus" && coordinator.CurrentOrder[1] == "Clock",
            "ReorderWidgets must swap widget indices.");
        Check(capturedOrder != null && capturedOrder[0] == "Focus" && capturedOrder[1] == "Clock",
            "OrderChanged callback must trigger with updated order.");

        // Verify widget context menu and removal item
        var firstWidget = coordinator.ActiveWidgets.Values.First();
        Check(coordinator.GetWidgetId(firstWidget) != null, "GetWidgetId must resolve widget id.");
        Check(firstWidget.ContextMenu?.Items.Count > 0, "Widget must have a context menu.");
        var removeItem = firstWidget.ContextMenu?.Items.OfType<MenuItem>().FirstOrDefault();
        Check(removeItem?.Header?.ToString() == UiText.Get("RemoveWidget"), "Widget context menu must have RemoveWidget header.");
        Check(removeItem?.Icon != null, "Widget remove item must have an icon.");

        // Verify DockSettingsStore validation for WidgetOrder
        var validSettings = new DockSettings(WidgetOrder: ["Clock", "Weather", "Media"]);
        Check(DockSettingsStore.Validate(validSettings), "Valid WidgetOrder must pass settings validation.");
        var duplicateSettings = new DockSettings(WidgetOrder: ["Clock", "Clock"]);
        Check(!DockSettingsStore.Validate(duplicateSettings), "Duplicate WidgetOrder must fail settings validation.");
        var whitespaceSettings = new DockSettings(WidgetOrder: ["Clock", "   "]);
        Check(!DockSettingsStore.Validate(whitespaceSettings), "Whitespace WidgetOrder must fail settings validation.");

        // Verify unified ClockWidget day, date, time and weather capsule
        var clockWidget = new ClockWidget();
        clockWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, ShowWeather: true));
        clockWidget.Activate();
        clockWidget.Tick(new DateTime(2026, 11, 16, 20, 16, 0));
        var dayText = clockWidget.FindName("DayOfWeekText") as TextBlock;
        var dateText = clockWidget.FindName("DateText") as TextBlock;
        var timeText = clockWidget.FindName("TimeText") as TextBlock;
        Check(dayText?.Text == "Mon", "ClockWidget must display concise sentence-case day of week.");
        Check(dateText?.Text == "Nov 16", "ClockWidget must display date format.");
        Check(timeText?.Text.Contains("20") == true || timeText?.Text.Contains("8") == true, "ClockWidget must display time.");
        clockWidget.Tick(new DateTime(2026, 11, 17, 20, 16, 0));
        Check(dayText?.Text == "Tue" && dateText?.Text == "Nov 17", "Date changes must refresh even when the displayed time is unchanged.");
        clockWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, DateStyle: ClockDateStyle.ShortDateOnly));
        clockWidget.Tick(new DateTime(2026, 11, 17, 20, 16, 0));
        var shortDate = dateText?.Text;
        clockWidget.ApplySettings(new DockSettings(Language: AppLanguage.English, DateStyle: ClockDateStyle.NumericDate));
        clockWidget.Tick(new DateTime(2026, 11, 17, 20, 16, 0));
        Check(shortDate != dateText?.Text, "Short date and numeric date must display different formats.");

        var focusClock = new TestClock();
        using var focusWidget = new FocusWidget(focusClock);
        focusWidget.ApplySettings(new DockSettings(FocusMinutes: 25));
        focusWidget.Activate();
        var focusPlay = (Button)focusWidget.FindName("PlayButton")!;
        var focusRemaining = (TextBlock)focusWidget.FindName("RemainingText")!;
        focusPlay.RaiseEvent(new RoutedEventArgs(ButtonBase.ClickEvent));
        focusClock.Advance(TimeSpan.FromMinutes(5));
        focusWidget.Tick(DateTime.Now.AddHours(-2));
        Check(focusRemaining.Text == "20:00", "Focus timer must use monotonic elapsed time.");
        focusPlay.RaiseEvent(new RoutedEventArgs(ButtonBase.ClickEvent));
        focusClock.Advance(TimeSpan.FromMinutes(10));
        focusWidget.Tick(DateTime.Now.AddHours(3));
        Check(focusRemaining.Text == "20:00", "Paused focus timer must ignore changes to the system clock.");
        clockWidget.Dispose();

        // Verify DockStack icon property and notifications
        var tempFolder = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "OpenTaskbarStackTest_" + Guid.NewGuid().ToString("N"));
        System.IO.Directory.CreateDirectory(tempFolder);
        try
        {
            var stack = new DockStack(tempFolder, Dispatcher.CurrentDispatcher);
            var iconNotified = false;
            stack.PropertyChanged += (_, e) => { if (e.PropertyName == nameof(DockStack.Icon)) iconNotified = true; };
            stack.Icon = new RenderTargetBitmap(16, 16, 96, 96, PixelFormats.Pbgra32);
            Check(iconNotified, "DockStack.Icon property change must raise PropertyChanged event.");
            Check(stack.Icon != null, "DockStack.Icon must hold assigned BitmapSource.");
            stack.Dispose();
        }
        finally
        {
            System.IO.Directory.Delete(tempFolder, recursive: true);
        }

        // Verify DockWidget RenderTransform initialization for drag-and-drop animation
        var dummyWidget = new ClockWidget();
        Check(dummyWidget.RenderTransform is TransformGroup tg && tg.Children.Count >= 2 && tg.Children[0] is ScaleTransform && tg.Children[1] is TranslateTransform,
            "DockWidget must initialize TransformGroup with ScaleTransform and TranslateTransform for fluid animations.");
        Check(dummyWidget.RenderTransformOrigin == new Point(0.5, 0.5),
            "DockWidget RenderTransformOrigin must be centered for balanced scaling.");
        dummyWidget.Dispose();

        // Verify WidgetCoordinator.HandlesWidget helper
        Check(WidgetCoordinator.HandlesWidget("Clock"), "HandlesWidget must return true for default widget Clock.");
        Check(WidgetCoordinator.HandlesWidget("Media"), "HandlesWidget must return true for default widget Media.");
        Check(!WidgetCoordinator.HandlesWidget("NonExistentWidget"), "HandlesWidget must return false for unknown widget.");

        // Verify IsInteractiveControl safely handles FrameworkContentElement (e.g. Run) without throwing
        var textBlock = new TextBlock();
        var run = new System.Windows.Documents.Run("test");
        textBlock.Inlines.Add(run);
        Check(!WidgetCoordinator.IsInteractiveControl(run), "Run inside TextBlock must not be interactive and must not throw.");
        Check(WidgetCoordinator.IsInteractiveControl(new Slider()), "Slider must be recognized as interactive.");

        // Verify frozen TransformGroup recovery in WidgetCoordinator and MainWindow
        var frozenGroup = new TransformGroup
        {
            Children = { new ScaleTransform(1, 1), new TranslateTransform(0, 0) }
        };
        frozenGroup.Freeze();
        Check(frozenGroup.IsFrozen, "Test fixture TransformGroup must be frozen.");
        var frozenWidget = new ClockWidget { RenderTransform = frozenGroup };
        var (widgetScale, widgetTranslate) = WidgetCoordinator.GetWidgetTransforms(frozenWidget);
        Check(!widgetScale.IsFrozen && !widgetTranslate.IsFrozen, "WidgetCoordinator must replace frozen transforms with mutable ones.");
        frozenWidget.Dispose();

        var frozenBorder = new Border { RenderTransform = frozenGroup };
        var (iconScale, iconTranslate) = MainWindow.GetIconTransforms(frozenBorder);
        Check(!iconScale.IsFrozen && !iconTranslate.IsFrozen, "MainWindow must replace frozen icon transforms with mutable ones.");

        // Verify Fullscreen video / window bounds detection (TaskbarReplacementSession.IsFullScreenBounds)
        const long wsCaption = 0x00C00000;
        var normalMon = new TaskbarReplacementSession.Rect(0, 0, 1920, 1080);
        var normalWork = new TaskbarReplacementSession.Rect(0, 0, 1920, 1032);
        var normalMaximizedWin = new TaskbarReplacementSession.Rect(-8, -8, 1928, 1040);
        Check(!TaskbarReplacementSession.IsFullScreenBounds(normalMaximizedWin, wsCaption, isZoomed: true, normalMon, normalWork),
            "Normal maximized window conforming to work area must not be detected as full screen.");

        var fullScreenVideoWin = new TaskbarReplacementSession.Rect(0, 0, 1920, 1080);
        Check(TaskbarReplacementSession.IsFullScreenBounds(fullScreenVideoWin, 0, isZoomed: false, normalMon, normalWork),
            "Fullscreen video covering monitor must be detected as full screen.");

        Check(TaskbarReplacementSession.IsFullScreenBounds(fullScreenVideoWin, wsCaption, isZoomed: true, normalMon, normalWork),
            "Fullscreen video spanning across reserved shelf must be detected as full screen even if browser retained caption style.");

        var highDpiMon = new TaskbarReplacementSession.Rect(0, 0, 2560, 1440);
        var highDpiWork = new TaskbarReplacementSession.Rect(0, 0, 2560, 1380);
        var highDpiVideoWin = new TaskbarReplacementSession.Rect(1, 0, 2559, 1440);
        Check(TaskbarReplacementSession.IsFullScreenBounds(highDpiVideoWin, 0, isZoomed: false, highDpiMon, highDpiWork),
            "High-DPI fullscreen video with subpixel rounding within 2px tolerance must be detected as full screen.");

        var autoHideMon = new TaskbarReplacementSession.Rect(0, 0, 1920, 1080);
        var autoHideWork = new TaskbarReplacementSession.Rect(0, 0, 1920, 1080);
        var autoHideMaximizedWin = new TaskbarReplacementSession.Rect(-8, -8, 1928, 1088);
        Check(!TaskbarReplacementSession.IsFullScreenBounds(autoHideMaximizedWin, wsCaption, isZoomed: true, autoHideMon, autoHideWork),
            "Captioned maximized window in auto-hide mode must not be detected as full screen.");
        Check(!TaskbarReplacementSession.IsFullScreenBounds(autoHideMaximizedWin, 0, isZoomed: true, autoHideMon, autoHideWork),
            "Borderless maximized Electron/Chromium window with resize overhang must not be suppressed as fullscreen.");

        // Maximized browser / custom chrome app without WS_CAPTION in auto-hide mode
        Check(!TaskbarReplacementSession.IsFullScreenBounds(autoHideMaximizedWin, 0, isZoomed: true, autoHideMon, autoHideWork),
            "Custom-frame maximized window (e.g. Chrome) in auto-hide mode must not be detected as full screen.");

        var autoHideVideoWin = new TaskbarReplacementSession.Rect(0, 0, 1920, 1080);
        Check(TaskbarReplacementSession.IsFullScreenBounds(autoHideVideoWin, 0, isZoomed: false, autoHideMon, autoHideWork),
            "Fullscreen video in auto-hide mode must be detected as full screen.");

        foreach (var shellClass in new[] { "Progman", "WorkerW", "Shell_TrayWnd", "LogonUI", "LockAppWindow" })
        {
            Check(TaskbarReplacementSession.IsExcludedFullScreenClass(shellClass, includeCoreWindow: false),
                $"Shell class '{shellClass}' must be excluded from fullscreen suppression.");
            Check(TaskbarReplacementSession.IsExcludedFullScreenClass(shellClass, includeCoreWindow: true),
                $"Shell class '{shellClass}' must remain excluded when CoreWindow filtering is enabled.");
        }
        Check(!TaskbarReplacementSession.IsExcludedFullScreenClass("Windows.UI.Core.CoreWindow", includeCoreWindow: false),
            "Windows.UI.Core.CoreWindow must only be excluded on the taskbar-button secondary check.");
        Check(TaskbarReplacementSession.IsExcludedFullScreenClass("Windows.UI.Core.CoreWindow", includeCoreWindow: true),
            "Windows.UI.Core.CoreWindow must be excluded when includeCoreWindow is true.");
        foreach (var appClass in new[] { "Chrome_WidgetWin_1", "ApplicationFrameWindow", "" })
        {
            Check(!TaskbarReplacementSession.IsExcludedFullScreenClass(appClass, includeCoreWindow: false),
                $"Application class '{appClass}' must not be excluded from fullscreen suppression.");
            Check(!TaskbarReplacementSession.IsExcludedFullScreenClass(appClass, includeCoreWindow: true),
                $"Application class '{appClass}' must not be excluded when includeCoreWindow is true.");
        }

        Check(UiText.Get("ErrorMediaControl") == "Cannot control the media session.",
            "ErrorMediaControl must return localized media control failure message.");

        // Verify optical glass styling resources and panel structures
        Check(app.FindResource("MenuMaterial") is Brush, "MenuMaterial brush must be available.");
        Check(app.FindResource("MenuStroke") is Brush, "MenuStroke brush must be available.");

        // Verify refined slender ScrollBar and expanded Settings content metrics
        var sbStyle = (Style)app.FindResource(typeof(ScrollBar));
        var widthSetter = sbStyle.Setters.OfType<Setter>().FirstOrDefault(s => s.Property == ScrollBar.WidthProperty);
        Check(widthSetter is not null && (double)widthSetter.Value <= 8, "ScrollBar width must be refined and slender (<= 8 DIP).");
        Check((double)app.FindResource("SettingsReadingWidth") >= 960, "SettingsReadingWidth must accommodate wider layout.");

        var panelFrame = new OpenTaskbar.Views.Widgets.WidgetPanelFrame { Heading = "Test" };
        Check(panelFrame.Content is Border outerBorder && outerBorder.Child is Grid bodyGrid && bodyGrid.Children.Count == 2 && bodyGrid.Children[0] is Border && bodyGrid.Children[1] is ScrollViewer,
            "WidgetPanelFrame must feature outer glass frame, top specular sheen, and scroll viewer.");

        var flyout = new DockFlyout();
        var flyoutAnchor = new Button();
        var flyoutContent = new TextBlock { Text = "FlyoutTest" };
        flyout.Show(flyoutAnchor, flyoutContent, animate: false);
        Check(flyout.IsOpen, "DockFlyout must open correctly.");
        flyout.Close();
        Check(!flyout.IsOpen, "DockFlyout must close correctly.");

        // Verify themed ComboBox and ComboBoxItem styling
        Check(app.FindResource(typeof(ComboBox)) is Style cbStyle && cbStyle.Setters.Count > 0, "ComboBox must have themed style.");
        Check(app.FindResource(typeof(ComboBoxItem)) is Style cbiStyle && cbiStyle.Setters.Count > 0, "ComboBoxItem must have themed style.");

        // Verify dynamic weather condition gradient backgrounds and WeatherReport metrics
        var clearBg = WeatherService.GetConditionBackground(0);
        var rainBg = WeatherService.GetConditionBackground(61);
        var snowBg = WeatherService.GetConditionBackground(71);
        var stormBg = WeatherService.GetConditionBackground(95);
        Check(clearBg is LinearGradientBrush && rainBg is LinearGradientBrush && snowBg is LinearGradientBrush && stormBg is LinearGradientBrush,
            "Weather conditions must provide distinct LinearGradientBrush dynamic backgrounds.");
        Check(clearBg.IsFrozen && rainBg.IsFrozen && snowBg.IsFrozen && stormBg.IsFrozen,
            "Weather condition backgrounds must be frozen for performance and thread safety.");
        Check(!ReferenceEquals(clearBg, rainBg), "Clear and rain condition backgrounds must differ.");

        // Verify ClockWidget time of day periods and themes
        Check(OpenTaskbar.Widgets.ClockWidget.GetPeriod(8) == OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Morning, "Hour 8 must be morning period.");
        Check(OpenTaskbar.Widgets.ClockWidget.GetPeriod(14) == OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Noon, "Hour 14 must be noon period.");
        Check(OpenTaskbar.Widgets.ClockWidget.GetPeriod(21) == OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Evening, "Hour 21 must be evening period.");
        Check(OpenTaskbar.Widgets.ClockWidget.GetPeriod(2) == OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Evening, "Hour 2 must be evening period.");

        var morningTheme = OpenTaskbar.Widgets.ClockWidget.GetTheme(OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Morning);
        var noonTheme = OpenTaskbar.Widgets.ClockWidget.GetTheme(OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Noon);
        var eveningTheme = OpenTaskbar.Widgets.ClockWidget.GetTheme(OpenTaskbar.Widgets.ClockWidget.TimeOfDayPeriod.Evening);
        Check(morningTheme.IconKey == "IconSun" && noonTheme.IconKey == "IconSun" && eveningTheme.IconKey == "IconMoon",
            "Morning and noon must use IconSun; evening must use IconMoon.");
        Check(morningTheme.Background.IsFrozen && noonTheme.Background.IsFrozen && eveningTheme.Background.IsFrozen,
            "Time of day backgrounds must be frozen for performance.");
        Check(morningTheme.Border.IsFrozen && noonTheme.Border.IsFrozen && eveningTheme.Border.IsFrozen,
            "Time of day borders must be frozen for performance.");

        var testDays = new[]
        {
            new WeatherDay(DateOnly.FromDateTime(DateTime.Today), 0, 10, 20),
            new WeatherDay(DateOnly.FromDateTime(DateTime.Today.AddDays(1)), 1, 11, 21),
            new WeatherDay(DateOnly.FromDateTime(DateTime.Today.AddDays(2)), 61, 9, 17),
            new WeatherDay(DateOnly.FromDateTime(DateTime.Today.AddDays(3)), 3, 12, 19)
        };
        var testReport = new WeatherReport("Istanbul", 18.5, 0, 18.0, testDays, Humidity: 68.0, WindSpeed: 12.5);
        Check(testReport.Humidity == 68.0 && testReport.WindSpeed == 12.5, "WeatherReport must track humidity and wind speed.");
        Check(testReport.Days.Count == 4, "WeatherReport must track 4 total days (today + 3 forecast days).");

        // Verify native Windows Toast notification registration and update localized strings
        Check(NotificationService.AppId == "OpenTaskbar", "NotificationService AUMID must be OpenTaskbar.");
        NotificationService.EnsureRegistered();
        Check(UiText.Format("UpdateToastBody", "v0.2.5").Contains("v0.2.5"), "UpdateToastBody must format version tag.");
        Check(!string.IsNullOrWhiteSpace(UiText.Get("UpdateAvailableTitle")), "UpdateAvailableTitle must not be empty.");
        Check(UiText.Format("UpdateDownloading", "v0.2.5").Contains("v0.2.5"), "UpdateDownloading must format version tag.");
        Check(!string.IsNullOrWhiteSpace(UiText.Get("UpdateFailed")), "UpdateFailed must not be empty.");
        Check(!string.IsNullOrWhiteSpace(UiText.Get("UpdateInProgress")), "UpdateInProgress must not be empty.");

        // Release assets now carry architecture in the name. Mirror the real v0.3.3 asset order (x64 first,
        // duplicate unversioned EXEs later) so a wrong "first .exe" pick is caught instead of passing by accident.
        var dualArchReleaseJson = """
        {
            "tag_name": "v0.3.3",
            "html_url": "https://github.com/alztrk/opentaskbar/releases/tag/v0.3.3",
            "assets": [
                { "name": "checksums.txt", "browser_download_url": "https://example/checksums.txt", "size": 388 },
                {
                    "name": "OpenTaskbar-v0.3.3-win-x64.exe",
                    "browser_download_url": "https://example/OpenTaskbar-v0.3.3-win-x64.exe",
                    "size": 172194045,
                    "digest": "sha256:e4cf5203c619db512c7300f69fb0162797a6b823726d9a9bc8b9690d4f0110c6"
                },
                { "name": "OpenTaskbar-v0.3.3-win-x64.zip", "browser_download_url": "https://example/x64.zip", "size": 70078577 },
                {
                    "name": "OpenTaskbar-v0.3.3-win-x86.exe",
                    "browser_download_url": "https://example/OpenTaskbar-v0.3.3-win-x86.exe",
                    "size": 164497661,
                    "digest": "sha256:bbf3698be014a38a440e3290fa68f95a47d7063990e634ae09e804c3ba52332e"
                },
                { "name": "OpenTaskbar-v0.3.3-win-x86.zip", "browser_download_url": "https://example/x86.zip", "size": 65974360 },
                { "name": "OpenTaskbar-x64.exe", "browser_download_url": "https://example/OpenTaskbar-x64.exe", "size": 172194045 },
                { "name": "OpenTaskbar-x86.exe", "browser_download_url": "https://example/OpenTaskbar-x86.exe", "size": 164497661 },
                { "name": "release-manifest.json", "browser_download_url": "https://example/release-manifest.json", "size": 1244 }
            ]
        }
        """;
        var x64Release = UpdateChecker.ParseRelease(dualArchReleaseJson, new Version(0, 3, 2), "x64");
        Check(x64Release?.DownloadUrl?.EndsWith("OpenTaskbar-v0.3.3-win-x64.exe", StringComparison.Ordinal) == true
            && x64Release.Size == 172194045
            && x64Release.Sha256?.EndsWith("0110c6", StringComparison.Ordinal) == true,
            "x64 updater must pick the x64 EXE with its own size and digest.");
        var x86Release = UpdateChecker.ParseRelease(dualArchReleaseJson, new Version(0, 3, 2), "x86");
        Check(x86Release?.DownloadUrl?.EndsWith("OpenTaskbar-v0.3.3-win-x86.exe", StringComparison.Ordinal) == true
            && x86Release.Size == 164497661
            && x86Release.Sha256?.EndsWith("52332e", StringComparison.Ordinal) == true,
            "x86 updater must pick the x86 EXE, never the x64 binary listed first.");
        Check(UpdateChecker.IsValidSha256Digest(x64Release?.Sha256)
            && UpdateChecker.IsValidSha256Digest(x86Release?.Sha256),
            "GitHub asset digests must pass the updater's SHA-256 format check.");

        // Old single-architecture releases and the legacy fixture must keep working.
        var testReleaseJson = """
        {
            "tag_name": "v0.3.0",
            "html_url": "https://github.com/alztrk/opentaskbar/releases/tag/v0.3.0",
            "assets": [
                {
                    "name": "OpenTaskbar.exe",
                    "browser_download_url": "https://github.com/alztrk/opentaskbar/releases/download/v0.3.0/OpenTaskbar.exe",
                    "size": 71373244
                }
            ]
        }
        """;
        var parsedNewer = UpdateChecker.ParseRelease(testReleaseJson, new Version(0, 2, 5), "x86");
        Check(parsedNewer is not null, "Newer version in release JSON must be recognized.");
        Check(parsedNewer!.Tag == "v0.3.0", "Parsed tag must match release tag.");
        Check(parsedNewer.DownloadUrl == "https://github.com/alztrk/opentaskbar/releases/download/v0.3.0/OpenTaskbar.exe",
            "Legacy OpenTaskbar.exe asset must still be selectable for old releases.");
        Check(parsedNewer.Size == 71373244, "Parsed asset size must match.");

        var parsedOlder = UpdateChecker.ParseRelease(testReleaseJson, new Version(0, 3, 5), "x64");
        Check(parsedOlder is null, "Older version in release JSON must return null.");

        var noAssetJson = """
        {
            "tag_name": "v0.4.0",
            "html_url": "https://github.com/alztrk/opentaskbar/releases/tag/v0.4.0",
            "assets": []
        }
        """;
        var parsedNoAsset = UpdateChecker.ParseRelease(noAssetJson, new Version(0, 2, 5));
        Check(parsedNoAsset is not null && parsedNoAsset.DownloadUrl is null,
            "Release without binary asset must have null DownloadUrl.");

        // Verify in-place update script generation
        var script = UpdateChecker.GenerateUpdaterScript(@"C:\temp\OpenTaskbar.exe", @"C:\app\OpenTaskbar.exe");
        Check(script.Contains(@"set ""SRC=C:\temp\OpenTaskbar.exe"""), "Script must set source path.");
        Check(script.Contains(@"set ""DST=C:\app\OpenTaskbar.exe"""), "Script must set destination path.");
        Check(script.Contains("copy /y"), "Script must perform copy /y.");
        Check(script.Contains("start \"\" \"%DST%\""), "Script must start updated executable.");
        Check(script.Contains("del /f /q \"%~f0\""), "Script must clean itself up.");

        // Shell shortcuts are deliberately NOT invoked here: OpenNotificationCenter/OpenQuickSettings synthesize
        // real Win+N / Win+A keystrokes, which opens Windows panels on the developer's desktop and can leave a Win
        // modifier stuck. Only the pure predicate is checked; anchoring is covered by the widget interaction tests.
        ShellShortcuts.IsMediaWidgetActive = () => true;
        Check(ShellShortcuts.IsMediaWidgetActive() == true, "IsMediaWidgetActive predicate must report active state.");
        ShellShortcuts.IsMediaWidgetActive = null;

        // Verify uninstall and dock item order pruning on widget removal
        var initialSettings = new DockSettings(
            Widgets: ["Clock", "Stopwatch", "Countdown", "Media"],
            InstalledWidgets: ["Clock", "Stopwatch", "Countdown", "Media", "Storage"],
            WidgetOrder: ["Clock", "Stopwatch", "Countdown", "Media"],
            DockItemOrder: ["start", "app:C:\\app.exe", "widget:Stopwatch", "widget:Clock", "widget:Countdown", "bin"],
            ShowClock: true,
            ShowMedia: true);

        void RemoveWidgetFromSettings(DockSettings settings, string id, out DockSettings updated)
        {
            var widgets = (settings.Widgets ?? [])
                .Where(w => !string.Equals(w, id, StringComparison.OrdinalIgnoreCase))
                .ToArray();
            var installed = (settings.InstalledWidgets ?? [])
                .Where(w => !string.Equals(w, id, StringComparison.OrdinalIgnoreCase))
                .ToArray();
            var order = settings.WidgetOrder?
                .Where(w => !string.Equals(w, id, StringComparison.OrdinalIgnoreCase))
                .ToArray();
            var dockItemOrder = settings.DockItemOrder?
                .Where(token => !string.Equals(token, "widget:" + id, StringComparison.OrdinalIgnoreCase))
                .ToArray();
            updated = settings with
            {
                Widgets = widgets,
                InstalledWidgets = installed,
                WidgetOrder = order,
                DockItemOrder = dockItemOrder,
                ShowClock = id.Equals("Clock", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowClock,
                ShowMedia = id.Equals("Media", StringComparison.OrdinalIgnoreCase) ? false : settings.ShowMedia
            };
        }

        RemoveWidgetFromSettings(initialSettings, "Stopwatch", out var afterLegacyRemoved);
        Check(afterLegacyRemoved.Widgets?.Contains("Stopwatch") == false, "Removed legacy widget must not be in Widgets.");
        Check(afterLegacyRemoved.InstalledWidgets?.Contains("Stopwatch") == false, "Removed legacy widget must be uninstalled from InstalledWidgets.");
        Check(afterLegacyRemoved.DockItemOrder?.Contains("widget:Stopwatch") == false, "Removed legacy widget must be pruned from DockItemOrder.");
        Check(afterLegacyRemoved.DockItemOrder?.SequenceEqual(["start", "app:C:\\app.exe", "widget:Clock", "widget:Countdown", "bin"]) == true,
            "Remaining dock items must preserve their order after legacy widget removal.");

        RemoveWidgetFromSettings(afterLegacyRemoved, "Clock", out var afterCoordinatorRemoved);
        Check(afterCoordinatorRemoved.Widgets?.Contains("Clock") == false, "Removed coordinator widget must not be in Widgets.");
        Check(afterCoordinatorRemoved.InstalledWidgets?.Contains("Clock") == false, "Removed coordinator widget must be uninstalled from InstalledWidgets.");
        Check(afterCoordinatorRemoved.DockItemOrder?.Contains("widget:Clock") == false, "Removed coordinator widget must be pruned from DockItemOrder.");
        Check(afterCoordinatorRemoved.ShowClock == false, "ShowClock flag must be cleared when Clock widget is uninstalled.");

        // Verify that legacy widgets can be integrated into DefaultDockOrder tokens
        var legacyWidgets = new OpenTaskbar.Models.DockWidgetItem[]
        {
            new OpenTaskbar.Services.StopwatchDockWidget(TimeProvider.System),
            new OpenTaskbar.Services.CountdownDockWidget(TimeProvider.System),
            new OpenTaskbar.Models.DockWidgetItem("Storage", "Storage")
        };
        var tokensWithLegacy = new List<string> { "start", "app:C:\\app.exe", "bin", "widget:Media" };
        foreach (var leg in legacyWidgets)
        {
            var tok = "widget:" + leg.Id;
            if (!tokensWithLegacy.Contains(tok, StringComparer.OrdinalIgnoreCase))
                tokensWithLegacy.Add(tok);
        }
        Check(tokensWithLegacy.Contains("widget:Stopwatch") && tokensWithLegacy.Contains("widget:Countdown") && tokensWithLegacy.Contains("widget:Storage"),
            "Default dock order must support legacy widget tokens alongside coordinator widgets.");
        Check(tokensWithLegacy.IndexOf("widget:Stopwatch") > tokensWithLegacy.IndexOf("widget:Media"),
            "Legacy widgets must follow the order tokens in the unified dock strip.");

        // Verify taskbar corner radius validation and automatic screen edge spacing
        var customGeometrySettings = new DockSettings(DockCornerRadius: 24);
        Check(OpenTaskbar.Services.DockSettingsStore.Validate(customGeometrySettings),
            "Valid DockCornerRadius must pass validation.");

        var sharpCornerSettings = new DockSettings(DockCornerRadius: 0);
        Check(OpenTaskbar.Services.DockSettingsStore.Validate(sharpCornerSettings),
            "Zero DockCornerRadius (sharp) must pass validation.");

        var invalidRadiusSettings = new DockSettings(DockCornerRadius: -5);
        Check(!OpenTaskbar.Services.DockSettingsStore.Validate(invalidRadiusSettings),
            "Negative DockCornerRadius must fail validation.");

        var excessiveRadiusSettings = new DockSettings(DockCornerRadius: 60);
        Check(!OpenTaskbar.Services.DockSettingsStore.Validate(excessiveRadiusSettings),
            "Excessive DockCornerRadius must fail validation.");

        foreach (var size in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
            foreach (var scale in new[] { 0.75, 1.0, 1.25, 1.5 })
            {
                var autoProfile = DockMetrics.For(size, scale);
                Check(autoProfile.BottomGap >= 2 && autoProfile.Shelf >= autoProfile.SurfaceHeight + autoProfile.BottomGap,
                    $"Screen edge spacing must be computed automatically from profile for {size}@{scale}.");
            }

        // Legacy settings files containing the removed DockBottomGap key must still load cleanly.
        var legacyJson = """{"DockCornerRadius":18,"DockBottomGap":12}""";
        var deserializedLegacy = System.Text.Json.JsonSerializer.Deserialize<DockSettings>(
            legacyJson, OpenTaskbar.Services.DockSettingsStore.SerializerOptions);
        Check(deserializedLegacy is { DockCornerRadius: 18 } && OpenTaskbar.Services.DockSettingsStore.Validate(deserializedLegacy),
            "Legacy settings JSON containing DockBottomGap must deserialize and validate cleanly.");

        // Verify macOS-style fisheye magnification math: smoothness, monotonicity, budget capping, and headroom safety.
        {
            var radius = DockFisheyeLayout.InfluenceRadiusForCell(50);
            Check(radius >= 110 && Math.Abs(radius - 140) < 0.01, "Influence radius must scale proportionally with cell size.");
            Check(Math.Abs(DockFisheyeLayout.ComputeFalloff(0, radius) - 1.0) < 1e-6, "Falloff must peak at 1.0 directly under the pointer.");
            Check(DockFisheyeLayout.ComputeFalloff(radius, radius) == 0.0 && DockFisheyeLayout.ComputeFalloff(radius + 20, radius) == 0.0,
                "Falloff must reach 0 at and beyond the influence radius.");
            var prevFalloff = 1.0;
            for (var d = 1.0; d <= radius; d += 1.0)
            {
                var nextFalloff = DockFisheyeLayout.ComputeFalloff(d, radius);
                Check(nextFalloff <= prevFalloff + 1e-9 && nextFalloff >= 0.0,
                    "Falloff must decrease monotonically with distance without jumps.");
                Check(Math.Abs(prevFalloff - nextFalloff) < 0.03,
                    "Falloff must be continuous with no single-pixel discontinuities across icon centers.");
                prevFalloff = nextFalloff;
            }

            // Spring must converge smoothly to both target peak and resting 1.0 without oscillating out of bounds.
            var val = 1.0;
            var vel = 0.0;
            var settledUp = false;
            for (var f = 0; f < 60; f++)
            {
                (val, vel, settledUp) = DockFisheyeLayout.StepSpring(val, DockMetrics.HoverScale, vel, 1.0 / 60.0, DockMetrics.HoverScale);
                Check(val >= 1.0 && val <= DockMetrics.HoverScale + 0.02, "Spring scale must stay bounded during bloom.");
            }
            Check(settledUp && Math.Abs(val - DockMetrics.HoverScale) < 0.005, "Spring must converge cleanly to target scale.");
            Check(vel == 0.0, "A settled spring must report zero velocity so the fisheye render loop can safely detach.");

            var settledDown = false;
            for (var f = 0; f < 60; f++)
            {
                (val, vel, settledDown) = DockFisheyeLayout.StepSpring(val, 1.0, vel, 1.0 / 60.0, DockMetrics.HoverScale);
                Check(val >= 1.0 && val <= DockMetrics.HoverScale + 0.02, "Spring scale must stay bounded during settle.");
            }
            Check(settledDown && Math.Abs(val - 1.0) < 0.005, "Spring must converge cleanly back to 1.0.");

            // Slot expansion must push outward proportionally, preserve non-overlapping visual slots, and never exceed budget.
            ReadOnlySpan<double> scales = [1.0, 1.25, 1.5, 1.25, 1.0];
            ReadOnlySpan<double> baseSizes = [50, 50, 50, 50, 50];
            Span<double> extras = stackalloc double[5];
            var unconstrained = DockFisheyeLayout.ComputeSlotExtras(scales, baseSizes, 500, extras);
            Check(unconstrained > 0 && extras[2] > extras[1] && Math.Abs(extras[1] - extras[3]) < 1e-6 && extras[0] == 0 && extras[4] == 0,
                "Slot expansion must be symmetric, peak at the hovered icon, and stay zero for unmagnified items.");
            Check(50 + extras[2] >= 38 * 1.5, "Peak expanded slot width must contain the magnified icon without overlapping neighbours.");

            var capped = DockFisheyeLayout.ComputeSlotExtras(scales, baseSizes, 20, extras);
            Check(capped <= 20.001 && capped >= 19.99, "Slot expansion must scale down smoothly to stay within available monitor budget.");

            var blocked = DockFisheyeLayout.ComputeSlotExtras(scales, baseSizes, 0, extras);
            Check(blocked == 0 && extras[2] == 0, "Zero expansion budget (scrolling or width-locked dock) must produce zero layout shift.");

            // Safe lift must stay inside DockHeadroom + vertical padding at every size and scale.
            foreach (var size in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
                foreach (var scale in new[] { 0.75, 1.0, 1.25, 1.5 })
                {
                    var p = DockMetrics.For(size, scale);
                    var availableHeadroom = DockMetrics.Headroom + p.VerticalPadding;
                    var lift = DockFisheyeLayout.ComputeSafeLift(DockMetrics.HoverScale, DockMetrics.HoverScale, p.Icon, availableHeadroom);
                    var growth = p.Icon * (DockMetrics.HoverScale - 1.0);
                    Check(lift >= 0 && growth + lift <= availableHeadroom + 0.01,
                        $"Magnified icon growth + lift must fit inside headroom for {size}@{scale}.");
                }

            // Expansion must map continuously to both peak scale and influence radius: no boolean cliff at 0.5 DIP.
            {
                Check(DockFisheyeLayout.ComputeExpansionFraction(50, DockMetrics.HoverScale, 0) == 0,
                    "No expansion budget must mean the constrained magnification peak.");
                Check(Math.Abs(DockFisheyeLayout.ComputeExpansionFraction(50, DockMetrics.HoverScale, 33.25) - 0.5) < 0.01
                      && DockFisheyeLayout.ComputeExpansionFraction(50, DockMetrics.HoverScale, 200) == 1.0,
                    "Expansion fraction must ramp smoothly from constrained to full magnification.");
                var constrainedRadius = DockFisheyeLayout.InfluenceRadiusForCell(50, constrained: true);
                var freeRadius = DockFisheyeLayout.InfluenceRadiusForCell(50);
                Check(Math.Abs(DockFisheyeLayout.InfluenceRadiusForExpansion(50, 0) - constrainedRadius) < 0.01
                      && Math.Abs(DockFisheyeLayout.InfluenceRadiusForExpansion(50, 1) - freeRadius) < 0.01,
                    "Influence radius endpoints must match the legacy radii.");
                var midRadius = DockFisheyeLayout.InfluenceRadiusForExpansion(50, 0.5);
                Check(midRadius > constrainedRadius && midRadius < freeRadius,
                    "Influence radius must interpolate instead of switching abruptly with the budget.");
            }

            // #127: a 1 px overflow must not turn a nearly free dock into a zero-budget constrained one.
            {
                var free = DockFisheyeLayout.FreeExpansion(maximum: 1000, used: 840, scrollableOverflow: 0);
                var onePixel = DockFisheyeLayout.FreeExpansion(maximum: 1000, used: 840, scrollableOverflow: 1);
                var heavy = DockFisheyeLayout.FreeExpansion(maximum: 1000, used: 840, scrollableOverflow: 500);
                Check(Math.Abs(free - onePixel) <= 0.5 && onePixel > 150,
                    "One pixel of overflow must leave almost all free space available for magnification.");
                Check(heavy == 0, "Genuine overflow larger than the free space must still disable expansion.");
                var peak = DockFisheyeLayout.PeakExpansionNeed(50, DockMetrics.HoverScale);
                Check(peak > 50 && peak < 80, "Reserved arch expansion must match the measured ~58 px snap size.");
            }

            // Constrained docks (zero expansion budget) must cap peak scale so adjacent icons never overlap.
            {
                var largeConstrained = DockFisheyeLayout.ComputeEffectiveMaxScale(50, 38, DockMetrics.HoverScale, 0);
                var largeFree = DockFisheyeLayout.ComputeEffectiveMaxScale(50, 38, DockMetrics.HoverScale, 400);
                var constrainedRadius = DockFisheyeLayout.InfluenceRadiusForCell(50, constrained: true);
                var neighbourScale = DockFisheyeLayout.ComputeTargetScale(50, constrainedRadius, largeConstrained);
                var combinedHalfWidths = 38 * (largeConstrained + neighbourScale) / 2.0;
                Check(largeConstrained > 1.15 && largeConstrained < DockMetrics.HoverScale,
                    "Constrained full-width dock must still magnify visibly while capping below full expansion scale.");
                Check(combinedHalfWidths <= 50.0 - DockFisheyeLayout.MinimumNoOverlapGapDips + 0.1,
                    "Constrained peak + neighbour icons must preserve a clean non-overlapping gap.");
                Check(Math.Abs(largeFree - DockMetrics.HoverScale) < 1e-6,
                    "Unconstrained floating dock must unlock full HoverScale.");

                var previewClearanceFull = DockFisheyeLayout.ComputePreviewClearance(38, DockMetrics.HoverScale);
                var previewClearanceConstrained = DockFisheyeLayout.ComputePreviewClearance(38, largeConstrained);
                Check(DockMetrics.PreviewGap + previewClearanceFull >= 38 * (DockMetrics.HoverScale - 1.0) + DockFisheyeLayout.MaxLiftDips + 9.0,
                    "Thumbnail preview must clear full magnified icon peak with comfortable breathing room.");
                Check(previewClearanceConstrained >= 0 && previewClearanceConstrained <= previewClearanceFull,
                    "Preview clearance must scale down proportionally when peak magnification is constrained.");
            }

            // A deferred post-scroll reset must re-arm only when nothing already restarted the effect and the
            // last pointer position is known. Otherwise the fisheye stays stalled until the user clicks.
            Check(DockFisheyeLayout.ShouldResumeAfterScroll(
                    hasMagnificationPointer: false, rendering: false, hasLastPointerPoint: true),
                "After scrolling settles, magnification must re-arm from the last pointer position.");
            Check(!DockFisheyeLayout.ShouldResumeAfterScroll(
                    hasMagnificationPointer: true, rendering: false, hasLastPointerPoint: true),
                "A real mouse move that already restarted magnification must not be interrupted by the scroll timer.");
            Check(!DockFisheyeLayout.ShouldResumeAfterScroll(
                    hasMagnificationPointer: false, rendering: true, hasLastPointerPoint: true),
                "An active fisheye render loop must not be reset by a late scroll timer tick.");
            Check(!DockFisheyeLayout.ShouldResumeAfterScroll(
                    hasMagnificationPointer: false, rendering: false, hasLastPointerPoint: false),
                "Without a known pointer position there is nothing to re-arm after scrolling.");

            // Issue #82: DockScrollViewer must suppress scroll indicators while magnification expands an unscrolled dock,
            // and ignore sub-pixel (<= 1.0 DIP) rounding overflow.
            {
                var content = new Border { Width = 320, Height = 38 };
                var prevBtn = new Button();
                var nextBtn = new Button();
                var viewer = new DockScrollViewer
                {
                    Style = (Style)app.FindResource("DockScrollViewerStyle"),
                    HoverHeadroom = DockMetrics.Headroom,
                    ContentInset = new Thickness(0),
                    Content = content,
                    Width = 200,
                    Height = 62
                };
                viewer.SetExternalIndicators(prevBtn, nextBtn);
                viewer.Measure(new Size(200, 62));
                viewer.Arrange(new Rect(0, 0, 200, 62));
                viewer.UpdateLayout();
                Check(nextBtn.Visibility == Visibility.Visible && prevBtn.Visibility == Visibility.Collapsed,
                    "Genuine horizontal overflow must show the next scroll indicator.");

                viewer.SuppressOverflowIndicators = true;
                Check(nextBtn.Visibility == Visibility.Collapsed && prevBtn.Visibility == Visibility.Collapsed && viewer.OpacityMask is null,
                    "SuppressOverflowIndicators must hide scroll indicators and edge fade while magnification runs from an unscrolled state.");

                viewer.SuppressOverflowIndicators = false;
                Check(nextBtn.Visibility == Visibility.Visible,
                    "Clearing SuppressOverflowIndicators must restore indicators when genuine overflow remains.");

                // Sub-pixel overflow (0.6 DIP) must not flicker the scroll indicator.
                content.Width = 200.6;
                viewer.Measure(new Size(200, 62));
                viewer.Arrange(new Rect(0, 0, 200, 62));
                viewer.UpdateLayout();
                Check(nextBtn.Visibility == Visibility.Collapsed && prevBtn.Visibility == Visibility.Collapsed,
                    "Sub-pixel overflow under 1 DIP must not show scroll indicators.");
            }

            // Issue #81 & #92: Compact dock with default/larger external bottom gap and border thickness must grow
            // its shelf so DockButton never receives a WPF LayoutClip, and Top gap must add directly to the reserved shelf.
            foreach (var bottomGap in new[] { 0d, 6d, 12d })
            {
                var compact = DockMetrics.For(DockSize.Compact);
                var requiredShelf = MainWindow.ComputeEffectiveShelfHeight(compact, isVertical: false, borderThickness: 1, externalBottomGap: bottomGap, externalTopGap: 0);
                var innerHeight = requiredShelf - bottomGap - 2 - compact.VerticalPadding * 2;
                Check(innerHeight >= compact.Cell,
                    $"Compact shelf ({requiredShelf} DIP) with bottom gap {bottomGap} DIP must leave at least {compact.Cell} DIP interior height so DockButton is never layout-clipped.");

                var withTopGap = MainWindow.ComputeEffectiveShelfHeight(compact, isVertical: false, borderThickness: 1, externalBottomGap: bottomGap, externalTopGap: 16);
                Check(Math.Abs((withTopGap - requiredShelf) - 16) < 0.01,
                    $"Top gap (16 DIP) must increase the reserved shelf height by 16 DIP so maximized windows stop above the dock (#92, got {withTopGap - requiredShelf}).");
            }

            // Issue #167: OpenTaskbar's own executable must not be pinnable as an external app.
            {
                const string selfPath = @"C:\Program Files\OpenTaskbar\OpenTaskbar.exe";
                Check(MainWindow.IsSelfExecutable(selfPath, selfPath),
                    "Exact match against OpenTaskbar.exe must be detected as self.");
                Check(MainWindow.IsSelfExecutable(@"c:\program files\opentaskbar\OPENTASKBAR.EXE", selfPath),
                    "Case-insensitive match against OpenTaskbar.exe must be detected as self.");
                Check(!MainWindow.IsSelfExecutable(@"C:\Windows\ImmersiveControlPanel\SystemSettings.exe", selfPath),
                    "External apps such as Windows Settings must not be treated as self.");
                Check(!MainWindow.IsSelfExecutable(@"appsfolder:windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel", selfPath),
                    "AppsFolder URIs must not throw or match self.");
                Check(!MainWindow.IsSelfExecutable(null, selfPath) && !MainWindow.IsSelfExecutable("", selfPath),
                    "Null or empty executable paths must not match self.");
            }

            // Issue #167: Pinning a shared-host running app (e.g. Windows Settings) must replace its running token
            // in DockItemOrder in-place without disturbing adjacent widgets, and unpinning must prune its token
            // without touching sibling shared-host apps or PWAs.
            {
                const string runningSettingsKey = @"C:\Windows\System32\ApplicationFrameHost.exe|windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";
                const string pinnedSettingsKey = @"C:\Windows\ImmersiveControlPanel\SystemSettings.exe";
                var initialOrder = new[]
                {
                    "start",
                    @"app:C:\Apps\Code.exe",
                    "app:" + runningSettingsKey,
                    "widget:Clock",
                    "widget:Audio",
                    "tray"
                };

                var afterPin = MainWindow.UpdateDockItemOrderOnPin(initialOrder, runningSettingsKey, pinnedSettingsKey);
                Check(afterPin is not null && afterPin.SequenceEqual([
                        "start",
                        @"app:C:\Apps\Code.exe",
                        "app:" + pinnedSettingsKey,
                        "widget:Clock",
                        "widget:Audio",
                        "tray"
                    ]),
                    "Pinning a shared-host app must replace its DockItemOrder token in place while preserving all widgets.");

                var pinnedSettingsApp = new DockApp("Settings", pinnedSettingsKey)
                {
                    AppUserModelId = "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel"
                };
                var afterUnpin = MainWindow.UpdateDockItemOrderOnUnpin(afterPin, pinnedSettingsApp);
                Check(afterUnpin is not null && afterUnpin.SequenceEqual([
                        "start",
                        @"app:C:\Apps\Code.exe",
                        "widget:Clock",
                        "widget:Audio",
                        "tray"
                    ]),
                    "Unpinning must remove the app's token from DockItemOrder so no empty gap or ghost slot remains.");

                var pwaOrder = new[]
                {
                    @"app:C:\Program Files\Edge\msedge_proxy.exe|--app-id=alpha",
                    @"app:C:\Program Files\Edge\msedge_proxy.exe|--app-id=beta",
                    "widget:Clock"
                };
                var pwaAlpha = new DockApp("Alpha", @"C:\Program Files\Edge\msedge_proxy.exe") { LaunchArguments = "--app-id=alpha" };
                var afterPwaUnpin = MainWindow.UpdateDockItemOrderOnUnpin(pwaOrder, pwaAlpha);
                Check(afterPwaUnpin is not null && afterPwaUnpin.SequenceEqual([
                        @"app:C:\Program Files\Edge\msedge_proxy.exe|--app-id=beta",
                        "widget:Clock"
                    ]),
                    "Unpinning one PWA must not remove a sibling PWA sharing the same proxy executable.");
            }

            // Issue #128 Finding #3a: Floating dock layered host window must shrink tightly around dock + magnification
            // headroom + shadow margin and stay centered, while preserving full-width, auto-hide, and multi-monitor bounds.
            {
                var blur36 = new System.Windows.Media.Effects.DropShadowEffect { BlurRadius = 36, ShadowDepth = 4, Opacity = 0.45 };
                Check(Math.Abs(MainWindow.ComputeShadowMarginPerSide(blur36) - 40.0) < 0.01,
                    "Shadow margin per side must cover BlurRadius + ShadowDepth for visible DropShadowEffect.");
                var hiddenShadow = new System.Windows.Media.Effects.DropShadowEffect { BlurRadius = 36, ShadowDepth = 4, Opacity = 0 };
                Check(MainWindow.ComputeShadowMarginPerSide(hiddenShadow) == 0.0
                    && MainWindow.ComputeShadowMarginPerSide(null) == 0.0,
                    "Transparent or null shadow effect must require zero extra layered-window shadow margin.");

                var centered = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1920, dockSurfaceExtent: 600,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Center, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(centered.IsShrunk && Math.Abs(centered.Extent - 763.0) < 0.01,
                    $"Centered floating dock must shrink 1920 DIP layered window to 763 DIP (got {centered.Extent}).");
                Check(Math.Abs((centered.Offset + centered.Extent / 2.0) - 960.0) < 0.01,
                    "Shrunk centered floating layered window must stay centered on the monitor.");

                var leftAligned = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1920, dockSurfaceExtent: 600,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Left, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(leftAligned.IsShrunk && leftAligned.Offset == 0 && Math.Abs(leftAligned.Extent - 723.0) < 0.01,
                    "Left-aligned floating dock must anchor at monitor Left with single-side shadow margin.");

                var rightAligned = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1920, dockSurfaceExtent: 600,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Right, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(rightAligned.IsShrunk && Math.Abs((rightAligned.Offset + rightAligned.Extent) - 1920.0) < 0.01
                    && Math.Abs(rightAligned.Extent - 723.0) < 0.01,
                    "Right-aligned floating dock must anchor at monitor Right with single-side shadow margin.");

                var secondaryMonitor = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 1920, monitorExtent: 2560, dockSurfaceExtent: 600,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Center, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(secondaryMonitor.IsShrunk
                    && secondaryMonitor.Offset >= 1920
                    && secondaryMonitor.Offset + secondaryMonitor.Extent <= 1920 + 2560
                    && Math.Abs((secondaryMonitor.Offset + secondaryMonitor.Extent / 2.0) - (1920 + 1280)) < 0.01,
                    "Secondary-monitor floating dock must remain centered within the secondary monitor bounds.");

                var verticalDock = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1080, dockSurfaceExtent: 420,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Center, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(verticalDock.IsShrunk && verticalDock.Extent < 600
                    && Math.Abs((verticalDock.Offset + verticalDock.Extent / 2.0) - 540.0) < 0.01,
                    "Vertical centered floating dock must shrink tightly and remain vertically centered.");

                var verticalTop = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1080, dockSurfaceExtent: 420,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Left, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(verticalTop.IsShrunk && verticalTop.Offset == 0 && Math.Abs(verticalTop.Extent - 543.0) < 0.01,
                    "Vertical top-aligned (DockPosition.Left) floating dock must anchor at monitor Top.");

                var verticalBottom = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1080, dockSurfaceExtent: 420,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Right, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(verticalBottom.IsShrunk && Math.Abs((verticalBottom.Offset + verticalBottom.Extent) - 1080.0) < 0.01
                    && Math.Abs(verticalBottom.Extent - 543.0) < 0.01,
                    "Vertical bottom-aligned (DockPosition.Right) floating dock must anchor at monitor Bottom.");

                var fullWidth = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1920, dockSurfaceExtent: 600,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Center, isFullWidth: true, preserveFullEdgeForAutoHide: false);
                Check(!fullWidth.IsShrunk && fullWidth.Offset == 0 && fullWidth.Extent == 1920,
                    "Full-width mode must preserve the full monitor layered-window span.");

                var autoHide = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1920, dockSurfaceExtent: 600,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Center, isFullWidth: false, preserveFullEdgeForAutoHide: true);
                Check(!autoHide.IsShrunk && autoHide.Offset == 0 && autoHide.Extent == 1920,
                    "Auto-hide mode must preserve the full monitor edge trigger span.");

                var overflowed = MainWindow.ComputeFloatingLayeredWindowBounds(
                    monitorOffset: 0, monitorExtent: 1920, dockSurfaceExtent: 1850,
                    surfaceMarginStart: 8, surfaceMarginEnd: 8, magnificationHeadroom: 67,
                    shadowMarginPerSide: 40, DockPosition.Center, isFullWidth: false, preserveFullEdgeForAutoHide: false);
                Check(!overflowed.IsShrunk && overflowed.Offset == 0 && overflowed.Extent == 1920,
                    "A floating dock whose needed span reaches the monitor width must clamp cleanly to the monitor.");
            }

            // Leaving the dock must release the magnified frame: a settled spring lands exactly on 1.0 instead of
            // keeping a sub-0.0001 residue, and the frame shrinks with the decaying slots instead of snapping.
            {
                Check(MainWindow.ShouldWriteSpringScale(1.00009, 1.0, settled: true),
                    "A settled spring must write its exact target even when the last step is below 0.0001.");
                Check(!MainWindow.ShouldWriteSpringScale(1.00009, 1.00005, settled: false),
                    "Mid-spring sub-0.0001 steps must still be skipped.");
                Check(!MainWindow.ShouldWriteSpringScale(1.0, 1.0, settled: true),
                    "A spring already at its target must not be rewritten.");

                Span<double> residueExtras = stackalloc double[11];
                ReadOnlySpan<double> residueScales = [1.00009, 1.00009, 1.00009, 1.00009, 1.00009, 1.00009, 1.00009, 1.00009, 1.00009, 1.00009, 1.00009];
                ReadOnlySpan<double> residueBases = [48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48];
                var residue = DockFisheyeLayout.ComputeSlotExtras(residueScales, residueBases, 200, residueExtras);
                Check(residue > 0.01,
                    "Residual 1.00009x scales across a dozen icons exceed the 0.01 clear threshold, which is why the exact settle write matters.");

                Check(Math.Abs(MainWindow.ScaleDecayingSurfaceDelta(50, 40, 40) - 50) < 0.001,
                    "The first decay frame must keep the full reserved delta.");
                Check(Math.Abs(MainWindow.ScaleDecayingSurfaceDelta(50, 40, 20) - 25) < 0.001,
                    "The frame delta must shrink in proportion to the remaining slot expansion.");
                Check(MainWindow.ScaleDecayingSurfaceDelta(50, 40, 0) == 0,
                    "The frame delta must reach zero when the slots are back at rest.");
                Check(MainWindow.ScaleDecayingSurfaceDelta(30, 40, 35) >= 30,
                    "The frame must never be narrower than the expansion the slots still need.");
                Check(MainWindow.ScaleDecayingSurfaceDelta(50, 0, 0) == 50,
                    "Without a decay baseline the reserved delta must be kept.");
            }

            // Issue #128 Finding #5: Magnification layout writes must skip sub-0.25 DIP churn mid-spring,
            // while still clearing overrides on zero-extra decay and snapping cleanly on the final settled frame.
            {
                Check(!MainWindow.ShouldApplyLayoutDelta(50.0, 50.0),
                    "Zero delta must skip magnification layout writes.");
                Check(!MainWindow.ShouldApplyLayoutDelta(50.0, 50.24),
                    "Sub-0.25 DIP deltas must skip magnification layout writes.");
                Check(MainWindow.ShouldApplyLayoutDelta(50.0, 50.25)
                    && MainWindow.ShouldApplyLayoutDelta(50.0, 49.75),
                    "Deltas of at least 0.25 DIP must apply magnification layout writes.");
                Check(MainWindow.ShouldApplyLayoutDelta(double.NaN, 50.0),
                    "Unset (NaN) layout properties must still accept their initial magnification write.");

                Check(!MainWindow.ShouldApplySlotLayoutWrite(50.0, 50.0, extra: 0.0, hasOverride: false, settled: false),
                    "Unmagnified button without override must never be written.");
                Check(MainWindow.ShouldApplySlotLayoutWrite(50.24, 50.0, extra: 0.0, hasOverride: true, settled: false),
                    "Previously overridden button decaying to zero extra must clear its override even when within 0.24 DIP of BaseSize.");
                Check(!MainWindow.ShouldApplySlotLayoutWrite(58.0, 58.18, extra: 8.18, hasOverride: true, settled: false),
                    "Mid-spring sub-0.25 DIP slot delta must be skipped.");
                Check(MainWindow.ShouldApplySlotLayoutWrite(58.0, 58.18, extra: 8.18, hasOverride: true, settled: true),
                    "Final settled frame must bypass the 0.25 DIP threshold and snap to the exact converged slot width.");
            }
        }

        return checks;
    }
}
