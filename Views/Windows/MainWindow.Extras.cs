using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>Dock items after the apps: folder stacks and the Recycle Bin.</summary>
public partial class MainWindow
{
    private const int StackEntryLimit = 16;
    private readonly ObservableCollection<DockStack> _stacks = [];
    private readonly DockFlyout _stackFlyout = new();
    private DispatcherTimer? _recycleBinTimer;
    private bool? _recycleBinFull;
    private bool _recycleBinQueryRunning;
    private bool _recycleBinQueryAgain;
    private Point _dragStackStartPoint;
    private DockStack? _draggedStack;

    private static string DownloadsFolder => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads");

    private void InitializeExtras()
    {
        StackItems.ItemsSource = _stacks;
        _stackFlyout.Closed += (_, _) => _autoHideController?.EvaluateState();
        _recycleBinTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher) { Interval = TimeSpan.FromSeconds(3) };
        _recycleBinTimer.Tick += (_, _) => UpdateRecycleBin();
        ApplyExtrasSettings();
    }

    private static readonly Brush StackTileBrush = CreateFrozenStackBrush(Color.FromArgb(0x35, 0xFF, 0xFF, 0xFF));
    private static readonly Brush StackTileStroke = CreateFrozenStackBrush(Color.FromArgb(0x55, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardShadowBrush1 = CreateFrozenStackBrush(Color.FromArgb(0x40, 0x00, 0x00, 0x00));
    private static readonly Brush CardShadowBrush2 = CreateFrozenStackBrush(Color.FromArgb(0x55, 0x00, 0x00, 0x00));
    private static readonly Brush CardDeckBackBrush = CreateFrozenStackBrush(Color.FromArgb(0x35, 0x1E, 0x24, 0x30));
    private static readonly Brush CardDeckBackStroke = CreateFrozenStackBrush(Color.FromArgb(0x35, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckMidBrush = CreateFrozenStackBrush(Color.FromArgb(0x3C, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckMidStroke = CreateFrozenStackBrush(Color.FromArgb(0x50, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckFrontBrush = CreateFrozenStackBrush(Color.FromArgb(0x50, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckFrontStroke = CreateFrozenStackBrush(Color.FromArgb(0x75, 0xFF, 0xFF, 0xFF));
    private static readonly Brush BadgeBackdropBrush = CreateFrozenStackBrush(Color.FromArgb(0xEB, 0x0F, 0x17, 0x2A));
    private static readonly Brush BadgeStrokeBrush = CreateFrozenStackBrush(Color.FromArgb(0x60, 0x38, 0xBD, 0xF8));
    private static readonly Brush BadgeIconBrush = CreateFrozenStackBrush(Color.FromArgb(0xFF, 0x38, 0xBD, 0xF8));

    private static readonly Brush FolderTabBrush = CreateFrozenStackBrush(Color.FromArgb(0xFF, 0x02, 0x84, 0xC7));
    private static readonly Brush FolderBackBrush = CreateFrozenStackBrush(Color.FromArgb(0xFF, 0x03, 0x69, 0xA1));
    private static readonly Brush FolderPaperBrush = CreateFrozenStackBrush(Color.FromArgb(0xD8, 0xF0, 0xF9, 0xFF));
    private static readonly Brush FolderFrontBrush = CreateFrozenGradient(
        Color.FromArgb(0xFF, 0x38, 0xBD, 0xF8),
        Color.FromArgb(0xFF, 0x02, 0x84, 0xC7));
    private static readonly Brush FolderFrontStroke = CreateFrozenStackBrush(Color.FromArgb(0x60, 0xFF, 0xFF, 0xFF));

    private static Brush CreateFrozenGradient(Color top, Color bottom)
    {
        var brush = new LinearGradientBrush(top, bottom, new Point(0, 0), new Point(0, 1));
        brush.Freeze();
        return brush;
    }

    private static Brush CreateFrozenStackBrush(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    private int StackIconPixels => ApplicationIconProvider.ComputeTargetPixels(Profile.Icon, VisualTreeHelper.GetDpi(this).DpiScaleX);

    private void ApplyExtrasSettings()
    {
        var showBin = _preferences.Current.ShowRecycleBin;
        RecycleBinButton.Visibility = showBin ? Visibility.Visible : Visibility.Collapsed;
        if (showBin)
        {
            UpdateRecycleBin();
            _recycleBinTimer?.Start();
        }
        else
        {
            _recycleBinTimer?.Stop();
        }

        var folders = ConfiguredStackFolders();
        if (!folders.SequenceEqual(_stacks.Select(stack => stack.Path), StringComparer.OrdinalIgnoreCase))
        {
            foreach (var stack in _stacks) stack.Dispose();
            _stacks.Clear();
            foreach (var folder in folders.Where(Directory.Exists))
            {
                var stack = new DockStack(folder, Dispatcher);
                stack.Icon = CreateStackIcon(stack, StackIconPixels);
                stack.ContentsChanged += () => stack.Icon = CreateStackIcon(stack, StackIconPixels);
                _stacks.Add(stack);
            }
        }

        ExtrasDivider.Visibility = (showBin || _stacks.Count > 0) && (VisiblePinnedCount > 0 || _runningApps.Count > 0)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateExtrasDividerVisibility()
    {
        ExtrasDivider.Visibility = (RecycleBinButton.Visibility == Visibility.Visible || _stacks.Count > 0)
            && (VisiblePinnedCount > 0 || _runningApps.Count > 0)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private IReadOnlyList<string> ConfiguredStackFolders() => _preferences.Current.StackFolders ?? [DownloadsFolder];

    private int ExtraButtonCount => _stacks.Count + (RecycleBinButton.Visibility == Visibility.Visible ? 1 : 0);

    private static readonly Geometry FolderBadgeGeometry = CreateFrozenGeometry("M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z");

    private static Geometry CreateFrozenGeometry(string pathData)
    {
        var geom = Geometry.Parse(pathData);
        geom.Freeze();
        return geom;
    }

    internal static BitmapSource RenderModernFolderIcon(int size)
    {
        var target = Math.Max(16, size);
        var scale = target / 128.0;
        var visual = new DrawingVisual();
        using (var dc = visual.RenderOpen())
        {
            // 1. Ambient drop shadow under folder
            var shadowRect = new Rect(14 * scale, 34 * scale, 100 * scale, 74 * scale);
            dc.DrawRoundedRectangle(CardShadowBrush1, null, new Rect(shadowRect.X, shadowRect.Y + 4 * scale, shadowRect.Width, shadowRect.Height), 16 * scale, 16 * scale);

            // 2. Folder Tab (back layer)
            var tabRect = new Rect(18 * scale, 24 * scale, 42 * scale, 22 * scale);
            dc.DrawRoundedRectangle(FolderTabBrush, null, tabRect, 8 * scale, 8 * scale);

            // 3. Folder Back Body
            var backBodyRect = new Rect(14 * scale, 30 * scale, 100 * scale, 76 * scale);
            dc.DrawRoundedRectangle(FolderBackBrush, null, backBodyRect, 14 * scale, 14 * scale);

            // 4. White paper sheet inside
            var paperRect = new Rect(24 * scale, 26 * scale, 80 * scale, 48 * scale);
            dc.DrawRoundedRectangle(FolderPaperBrush, null, paperRect, 6 * scale, 6 * scale);

            // 5. Front Pocket with rich modern gradient and top highlight
            var frontRect = new Rect(14 * scale, 46 * scale, 100 * scale, 60 * scale);
            var frontPen = new Pen(FolderFrontStroke, Math.Max(0.5, 1.0 * scale));
            frontPen.Freeze();
            dc.DrawRoundedRectangle(FolderFrontBrush, frontPen, frontRect, 14 * scale, 14 * scale);
        }

        var rtb = new RenderTargetBitmap(target, target, 96, 96, PixelFormats.Pbgra32);
        rtb.Render(visual);
        rtb.Freeze();
        return rtb;
    }

    internal static BitmapSource? CreateStackIcon(DockStack stack, int targetSize = 128)
    {
        var size = Math.Max(16, targetSize);
        var icon = ApplicationIconProvider.GetForShellItem(stack.Path, size, iconOnly: true)
                   ?? ApplicationIconProvider.GetForFile(stack.Path, size)
                   ?? RenderModernFolderIcon(size);
        return icon is null ? null : FillSquare(icon, size);
    }

    /// <summary>
    /// Windows shell folder icons leave large transparent margins (measured 112x88 visible pixels in a
    /// 128x128 icon, i.e. 88% wide and only 69% tall) while application icons fill the whole square.
    /// That makes every folder look smaller than its neighboring app icons. Trim the transparent bounds
    /// and stretch the artwork to fill the same square so both icon kinds match visually.
    /// </summary>
    private static BitmapSource FillSquare(BitmapSource icon, int size)
    {
        try
        {
            var bgra = new FormatConvertedBitmap(icon, PixelFormats.Bgra32, null, 0);
            var width = bgra.PixelWidth;
            var height = bgra.PixelHeight;
            var stride = width * 4;
            var pixels = new byte[stride * height];
            bgra.CopyPixels(pixels, stride, 0);

            int minX = width, minY = height, maxX = -1, maxY = -1;
            for (var y = 0; y < height; y++)
            {
                var row = y * stride;
                for (var x = 0; x < width; x++)
                {
                    if (pixels[row + x * 4 + 3] <= 16) continue;
                    if (x < minX) minX = x;
                    if (x > maxX) maxX = x;
                    if (y < minY) minY = y;
                    if (y > maxY) maxY = y;
                }
            }

            if (maxX < minX || maxY < minY) return icon;
            var crop = new CroppedBitmap(bgra, new Int32Rect(minX, minY, maxX - minX + 1, maxY - minY + 1));
            var visual = new DrawingVisual();
            RenderOptions.SetBitmapScalingMode(visual, BitmapScalingMode.HighQuality);
            using (var dc = visual.RenderOpen())
            {
                dc.DrawImage(crop, new Rect(0, 0, size, size));
            }

            var result = new RenderTargetBitmap(size, size, 96, 96, PixelFormats.Pbgra32);
            result.Render(visual);
            result.Freeze();
            return result;
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Stack icon resize failed: {ex.Message}");
            return icon;
        }
    }

    private IEnumerable<Button> GetExtraDockButtons()
    {
        foreach (var stack in _stacks)
        {
            if (StackItems.ItemContainerGenerator.ContainerFromItem(stack) is DependencyObject container
                && FindVisualChild<Button>(container) is { } button)
            {
                yield return button;
            }
        }

        if (RecycleBinButton.Visibility == Visibility.Visible) yield return RecycleBinButton;
    }

    /// <summary>
    /// Refreshes the Recycle Bin state off the UI thread. SHQueryRecycleBin walks the bin on every drive and takes
    /// around 100 ms inside the dock process; polled every 3 s on the UI thread it froze the whole dock each time.
    /// Requests that arrive while a query runs are coalesced into one follow-up query.
    /// </summary>
    private async void UpdateRecycleBin()
    {
        if (_recycleBinQueryRunning)
        {
            _recycleBinQueryAgain = true;
            return;
        }

        _recycleBinQueryRunning = true;
        try
        {
            do
            {
                _recycleBinQueryAgain = false;
                var count = await Task.Run(RecycleBin.ItemCount);
                ApplyRecycleBinCount(count);
            }
            while (_recycleBinQueryAgain);
        }
        finally
        {
            _recycleBinQueryRunning = false;
        }
    }

    private void ApplyRecycleBinCount(long? count)
    {
        var full = count > 0;
        if (_recycleBinFull == full) return;
        _recycleBinFull = full;
        RefreshRecycleBinIcon();
        EmptyRecycleBinItem.IsEnabled = full;
    }

    private void RefreshRecycleBinIcon()
    {
        var full = _recycleBinFull == true;
        var raw = full ? RecycleBin.FullIcon : RecycleBin.EmptyIcon;
        var themed = ThemePacks.ApplyIcon(raw, null,
            specialKey: full ? IconPack.RecycleBinFullKey : IconPack.RecycleBinKey);
        RecycleBinIcon.Source = themed;
    }

    private void RecycleBin_Click(object sender, RoutedEventArgs e)
    {
        _thumbnailPreview.Hide();
        if (sender is Button button) AnimateActivationFeedback(button);
        ReportResult(RecycleBin.Open());
    }

    private void EmptyRecycleBin_Click(object sender, RoutedEventArgs e)
    {
        ReportResult(RecycleBin.Empty(_source?.Handle ?? 0));
        _recycleBinFull = null;
        UpdateRecycleBin();
    }

    private void RecycleBin_DragOver(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out _))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }

        if (e.Data.GetDataPresent(typeof(StartButtonDragData)))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = false;
            return;
        }

        e.Effects = e.Data.GetDataPresent(DataFormats.FileDrop) ? DragDropEffects.Move : DragDropEffects.None;
        e.Handled = true;
    }

    private void RecycleBin_Drop(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out var droppedToken))
        {
            MoveDockItem(droppedToken, GetDockInsertionIndex(e));
            e.Handled = true;
            return;
        }

        e.Handled = true;
        if (e.Data.GetData(DataFormats.FileDrop) is not string[] { Length: > 0 } paths) return;
        // Let the drag source finish before Windows shows its own confirmation UI.
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            ReportResult(RecycleBin.Delete(paths, _source?.Handle ?? 0));
            _recycleBinFull = null;
            UpdateRecycleBin();
        });
    }

    private static void ReportResult(string? error)
    {
        if (error is not null) ReportError(error);
    }

    private void Stack_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { DataContext: DockStack stack } button) return;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        ResetMagnification();
        stack.HasNewItems = false;
        _stackFlyout.Toggle(button, () => BuildStackContent(stack), _preferences.Current.AnimationsEnabled);
    }

    private UIElement BuildStackContent(DockStack stack)
    {
        var root = new StackPanel { Width = 4 * 96 };

        // 1. Sleek Header Bar with Folder Icon, Stack Name & Count Badge
        var header = new DockPanel { Margin = new Thickness(4, 2, 4, 10) };
        var entries = stack.RecentEntries(StackEntryLimit);

        var countBadge = new Border
        {
            CornerRadius = new CornerRadius(9),
            Padding = new Thickness(7, 2, 7, 2),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center
        };
        countBadge.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
        var countText = new TextBlock
        {
            Text = entries.Count.ToString(), FontSize = 11, FontWeight = FontWeights.SemiBold
        };
        countText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        countBadge.Child = countText;
        DockPanel.SetDock(countBadge, Dock.Right);
        header.Children.Add(countBadge);

        var titleGroup = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var folderIcon = new ContentControl
        {
            Content = Application.Current?.TryFindResource("IconFolder"),
            Width = 15, Height = 15, Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        folderIcon.SetResourceReference(FrameworkElement.StyleProperty, "WidgetIcon");
        folderIcon.SetResourceReference(Control.ForegroundProperty, "WidgetAccent");

        var titleText = new TextBlock
        {
            Text = stack.Name, FontWeight = FontWeights.SemiBold, FontSize = 13.5,
            VerticalAlignment = VerticalAlignment.Center
        };
        titleText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        titleGroup.Children.Add(folderIcon);
        titleGroup.Children.Add(titleText);
        header.Children.Add(titleGroup);

        root.Children.Add(header);

        var divider = new System.Windows.Shapes.Rectangle
        {
            Height = 1, Margin = new Thickness(2, 0, 2, 10)
        };
        divider.SetResourceReference(System.Windows.Shapes.Shape.FillProperty, "StrokeSubtle");
        root.Children.Add(divider);

        if (entries.Count == 0)
        {
            root.Children.Add(DockFlyout.Caption(UiText.Get("StackEmpty")));
        }

        var flyoutIconPixels = ApplicationIconProvider.ComputeTargetPixels(48, VisualTreeHelper.GetDpi(this).DpiScaleX);
        var grid = new WrapPanel();
        foreach (var entry in entries)
        {
            var image = new Image { Width = 46, Height = 46, Margin = new Thickness(0, 4, 0, 6) };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var label = new TextBlock
            {
                Text = entry.Name, TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap,
                TextTrimming = TextTrimming.CharacterEllipsis, MaxHeight = 32, FontSize = 11
            };
            label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

            var tile = new StackPanel { Width = 84 };
            tile.Children.Add(image);
            tile.Children.Add(label);
            var path = entry.FullName;
            var button = DockFlyout.FlatButton(tile, (_, _) =>
            {
                _stackFlyout.Close();
                OpenPath(path);
            }, entry.Name);
            button.Padding = new Thickness(4, 6, 4, 6);
            button.Margin = new Thickness(2);
            grid.Children.Add(button);
            // Thumbnails can be slow (videos, network drives); fill them in after the flyout is shown.
            Dispatcher.BeginInvoke(DispatcherPriority.Background, () => image.Source = ApplicationIconProvider.GetForShellItem(path, flyoutIconPixels));
        }

        root.Children.Add(grid);

        // 2. Modern Footer Bar
        var footerPanel = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Center };
        var openFolderIcon = new ContentControl
        {
            Content = Application.Current?.TryFindResource("IconFolder"),
            Width = 14, Height = 14, Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        openFolderIcon.SetResourceReference(FrameworkElement.StyleProperty, "WidgetIcon");
        openFolderIcon.SetResourceReference(Control.ForegroundProperty, "TextPrimary");

        var footerText = new TextBlock
        {
            Text = UiText.Get("OpenFolderInExplorer"), FontWeight = FontWeights.SemiBold, FontSize = 11.5
        };
        footerText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        footerPanel.Children.Add(openFolderIcon);
        footerPanel.Children.Add(footerText);

        var open = DockFlyout.FlatButton(footerPanel, (_, _) =>
        {
            _stackFlyout.Close();
            OpenPath(stack.Path);
        });
        open.Margin = new Thickness(0, 10, 0, 0);
        open.Padding = new Thickness(8, 7, 8, 7);
        open.HorizontalAlignment = HorizontalAlignment.Stretch;
        open.HorizontalContentAlignment = HorizontalAlignment.Center;
        root.Children.Add(open);
        return root;
    }

    private void OpenPath(string path)
    {
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(path) { UseShellExecute = true });
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException or FileNotFoundException)
        {
            AppLog.Warning($"Cannot open {path}: {ex.Message}");
            ReportError("ErrorOpenFileOrFolder");
        }
    }

    private void StackOpenFolder_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: DockStack stack }) OpenPath(stack.Path);
    }

    private void StackRemove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { DataContext: DockStack stack }) return;
        var folders = ConfiguredStackFolders()
            .Where(folder => !string.Equals(folder, stack.Path, StringComparison.OrdinalIgnoreCase)).ToList();
        SaveStackFolders(folders);
    }

    private void AddStack_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new Microsoft.Win32.OpenFolderDialog { Title = UiText.Get("AddStack"), Multiselect = false };
        if (dialog.ShowDialog() != true) return;
        var folders = ConfiguredStackFolders().ToList();
        if (folders.Contains(dialog.FolderName, StringComparer.OrdinalIgnoreCase)) return;
        folders.Add(dialog.FolderName);
        SaveStackFolders(folders);
    }

    private void SaveStackFolders(IReadOnlyList<string> folders)
    {
        var error = _preferences.Save(_preferences.Current with { StackFolders = folders });
        if (error is not null) ReportError(error);
    }

    private void StackButton_PreviewMouseLeftButtonDown(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        if (sender is Button { DataContext: DockStack stack } && _stacks.Contains(stack))
        {
            _dragStackStartPoint = e.GetPosition(this);
            _draggedStack = stack;
        }
        else
        {
            _draggedStack = null;
        }
    }

    private void StackButton_PreviewMouseLeftButtonUp(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        _draggedStack = null;
    }

    private void AnimateStackDrag(Button button, DockStack stack)
    {
        var icon = GetButtonIcon(button);
        var (scale, translate) = icon != null ? GetIconTransforms(icon) : (null, null);
        var animDuration = TimeSpan.FromMilliseconds(130);
        var easeOut = new CubicEase { EasingMode = EasingMode.EaseOut };

        button.Opacity = 0.55;
        if (scale is not null && translate is not null && _preferences.Current.AnimationsEnabled && SystemParameters.ClientAreaAnimation)
        {
            scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(1.10, animDuration) { EasingFunction = easeOut });
            scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(1.10, animDuration) { EasingFunction = easeOut });
            translate.BeginAnimation(IsVertical ? TranslateTransform.XProperty : TranslateTransform.YProperty,
                new DoubleAnimation(-4 * BounceDirection, animDuration) { EasingFunction = easeOut });
        }

        try
        {
            DragDrop.DoDragDrop(button, stack, DragDropEffects.Move);
        }
        finally
        {
            button.Opacity = 1.0;
            ResetDisplacements();
            if (scale is not null && translate is not null)
            {
                scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(1.0, animDuration) { EasingFunction = easeOut });
                scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(1.0, animDuration) { EasingFunction = easeOut });
                translate.BeginAnimation(IsVertical ? TranslateTransform.XProperty : TranslateTransform.YProperty,
                    new DoubleAnimation(0, animDuration) { EasingFunction = easeOut });
            }
        }
    }

    private void StackButton_PreviewMouseMove(object sender, System.Windows.Input.MouseEventArgs e)
    {
        if (e.LeftButton != System.Windows.Input.MouseButtonState.Pressed || _draggedStack is null || sender is not Button button)
        {
            return;
        }

        var currentPoint = e.GetPosition(this);
        var diff = _dragStackStartPoint - currentPoint;
        if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
        {
            var stack = _draggedStack;
            _draggedStack = null;
            AnimateStackDrag(button, stack);
        }
    }

    private void StackButton_DragOver(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out _))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }

        if (e.Data.GetDataPresent(typeof(StartButtonDragData)))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = false;
            return;
        }

        if (e.Data.GetDataPresent(typeof(DockStack))
            && sender is Button targetButton
            && targetButton.DataContext is DockStack targetStack
            && _stacks.Contains(targetStack))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;

            if (_preferences.Current.AnimationsEnabled && SystemParameters.ClientAreaAnimation)
            {
                UpdateDragDisplacements(targetButton, e);
            }
        }
        else
        {
            e.Effects = DragDropEffects.None;
            e.Handled = true;
        }
    }

    private void StackButton_DragLeave(object sender, DragEventArgs e)
    {
        if (sender is Button button && GetButtonIcon(button) is { } icon)
        {
            ResetIconDisplacement(icon);
        }
    }

    private void StackButton_Drop(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out var droppedToken))
        {
            MoveDockItem(droppedToken, GetDockInsertionIndex(e));
            e.Handled = true;
            return;
        }

        if (e.Data.GetData(typeof(DockStack)) is DockStack sourceStack
            && sender is Button targetButton
            && targetButton.DataContext is DockStack targetStack
            && _stacks.Contains(sourceStack)
            && _stacks.Contains(targetStack)
            && !ReferenceEquals(sourceStack, targetStack))
        {
            var oldIndex = _stacks.IndexOf(sourceStack);
            var newIndex = _stacks.IndexOf(targetStack);
            if (oldIndex >= 0 && newIndex >= 0 && oldIndex != newIndex)
            {
                ResetDisplacements();
                _stacks.Move(oldIndex, newIndex);
                SaveStackFolders(_stacks.Select(s => s.Path).ToList());
                AnimateDropSettle(targetButton);
            }
        }
        ResetDisplacements();
        e.Handled = true;
    }

    /// <summary>Pins apps from a settings backup that are not pinned yet and exist on this PC; returns how many were added.</summary>
    internal int ImportPins(IReadOnlyList<PinBackup> pins)
    {
        var added = 0;
        foreach (var pin in pins)
        {
            if (!File.Exists(pin.ExecutablePath)
                || _pinnedApps.Any(app => string.Equals(app.ExecutablePath, pin.ExecutablePath, StringComparison.OrdinalIgnoreCase)))
            {
                continue;
            }

            var (shortcutPath, error) = WindowsTaskbarPinImporter.AddPin(pin.Name, pin.ExecutablePath, pin.Arguments);
            if (error is not null || shortcutPath is null)
            {
                AppLog.Warning($"Import pin {pin.ExecutablePath}: {error}");
                continue;
            }

            _pinnedApps.Add(new DockApp(pin.Name, pin.ExecutablePath)
            {
                Icon = ApplicationIconProvider.GetForFile(pin.ExecutablePath),
                LaunchArguments = pin.Arguments,
                ShortcutPath = shortcutPath
            });
            added++;
        }

        if (added > 0)
        {
            if (_windowTracker is not null) UpdateRunningApps(VisibleSnapshot);
            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        }

        return added;
    }

    private void DisposeExtras()
    {
        _recycleBinTimer?.Stop();
        _stackFlyout.Close();
        foreach (var stack in _stacks) stack.Dispose();
        _stacks.Clear();
    }
}
