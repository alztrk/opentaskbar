using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>A keyboard-accessible grid of live windows, centered in the invoking display's work area.</summary>
internal sealed class AppWindowPicker : Window
{
    private const double Gap = 12;
    private const double FramePadding = 18;
    private static readonly Style PickerButtonStyle = CreateButtonStyle();
    private readonly DisplayInfo _display;
    private readonly BitmapSource? _icon;
    private readonly Action<nint> _activate;
    private readonly UniformGrid _cards = new();
    private readonly DockPanel _paging = new() { Margin = new Thickness(0, 14, 0, 0) };
    private readonly TextBlock _pageLabel = new() { VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _previous;
    private readonly Button _next;
    private readonly List<(Button Button, Border Preview, FrameworkElement Fallback, TrackedWindow Window)> _visible = [];
    private readonly List<nint> _thumbnails = [];
    private readonly int _columns;
    private readonly int _pageSize;
    private readonly double _cardWidth;
    private readonly double _cardHeight;
    private IReadOnlyList<TrackedWindow> _windows = [];
    private int _page;
    private nint _selected;
    private nint _handle;
    private bool _closed;
    private bool _thumbnailRefreshQueued;

    internal AppWindowPicker(string title, BitmapSource? icon, DisplayInfo display, IReadOnlyList<TrackedWindow> windows,
        Action<nint> activate)
    {
        _display = display;
        _icon = icon;
        _activate = activate;
        Title = UiText.Get("WindowPickerTitle");
        AutomationProperties.SetName(this, Title);
        WindowStyle = WindowStyle.None;
        ResizeMode = ResizeMode.NoResize;
        ShowInTaskbar = false;
        ShowActivated = true;
        Topmost = true;
        WindowStartupLocation = WindowStartupLocation.Manual;
        UseLayoutRounding = true;
        SetResourceReference(BackgroundProperty, "SurfacePopup");
        SetResourceReference(FontFamilyProperty, "InterfaceFont");

        var width = Math.Max(1, display.WorkArea.Width - 36 - FramePadding * 2);
        var height = Math.Max(1, display.WorkArea.Height - 36 - FramePadding * 2 - 96);
        _columns = Math.Min(windows.Count, Math.Clamp((int)((width + Gap) / (242 + Gap)), 1, 3));
        _cardWidth = Math.Min(312, (width - Gap * (_columns - 1)) / _columns);
        _cardHeight = Math.Min(222, Math.Max(64, height));
        var rows = Math.Clamp((int)((height + Gap) / (_cardHeight + Gap)), 1, 3);
        _pageSize = rows * _columns;
        _cards.Columns = _columns;

        var titleText = Text(title, 21, "TextPrimary");
        titleText.FontWeight = FontWeights.SemiBold;
        titleText.TextTrimming = TextTrimming.CharacterEllipsis;
        titleText.VerticalAlignment = VerticalAlignment.Center;
        var close = ActionButton("×", UiText.Get("Close"), Close);
        close.FontSize = 19;
        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 14) };
        DockPanel.SetDock(close, Dock.Right);
        header.Children.Add(close);
        header.Children.Add(titleText);

        _previous = ActionButton("‹", UiText.Get("WindowPickerPrevious"), () => ChangePage(-1));
        _next = ActionButton("›", UiText.Get("WindowPickerNext"), () => ChangePage(1));
        DockPanel.SetDock(_previous, Dock.Left);
        DockPanel.SetDock(_next, Dock.Right);
        _paging.Children.Add(_previous);
        _paging.Children.Add(_next);
        _pageLabel.HorizontalAlignment = HorizontalAlignment.Center;
        _pageLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        _paging.Children.Add(_pageLabel);

        var stack = new StackPanel { Children = { header, _cards, _paging } };
        var frame = new Border { Child = stack, Padding = new Thickness(FramePadding), BorderThickness = new Thickness(1) };
        frame.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        Content = frame;
        PreviewKeyDown += HandleKeys;
        Deactivated += (_, _) => Dispatcher.BeginInvoke(DispatcherPriority.Background, new Action(() =>
        {
            if (!_closed && !IsActive) Close();
        }));
        ContentRendered += (_, _) =>
        {
            FocusSelection();
            QueueThumbnails();
        };
        DpiChanged += (_, _) => QueueThumbnails();
        Refresh(windows);
    }

    internal static IReadOnlyList<TrackedWindow> OrderWindows(IEnumerable<TrackedWindow> windows, DockApp app,
        nint monitor, Func<nint, nint> monitorForWindow)
    {
        return windows.Where(window => window.Handle != 0 && Matches(app, window))
            .DistinctBy(window => window.Handle)
            .OrderByDescending(window => monitorForWindow(window.Handle) == monitor)
            .ToList();
    }

    private static bool Matches(DockApp app, TrackedWindow window)
    {
        if (!string.IsNullOrWhiteSpace(app.AppUserModelId) && !string.IsNullOrWhiteSpace(window.AppUserModelId))
            return string.Equals(app.AppUserModelId, window.AppUserModelId, StringComparison.OrdinalIgnoreCase);
        return string.Equals(app.ExecutablePath, window.ExecutablePath, StringComparison.OrdinalIgnoreCase);
    }

    internal void Refresh(IReadOnlyList<TrackedWindow> windows)
    {
        if (_closed || _windows.SequenceEqual(windows)) return;
        if (windows.Count == 0) { Close(); return; }
        _windows = windows;
        var index = Array.FindIndex(windows.ToArray(), window => window.Handle == _selected);
        _page = index >= 0 ? index / _pageSize : Math.Min(_page, (_windows.Count - 1) / _pageSize);
        BuildPage(index >= 0 ? index % _pageSize : 0);
    }

    private void ChangePage(int step)
    {
        var page = Math.Clamp(_page + step, 0, (_windows.Count - 1) / _pageSize);
        if (page == _page) return;
        _page = page;
        BuildPage(0);
    }

    private void BuildPage(int selected)
    {
        ReleaseThumbnails();
        _visible.Clear();
        _cards.Children.Clear();
        foreach (var window in _windows.Skip(_page * _pageSize).Take(_pageSize))
        {
            var name = string.IsNullOrWhiteSpace(window.Title) ? Title : window.Title;
            var image = new Image { Source = _icon, Width = 36, Height = 36, Stretch = Stretch.Uniform };
            var placeholder = Text(UiText.Get("PreviewUnavailable"), 11, "TextSecondary");
            placeholder.Margin = new Thickness(0, 8, 0, 0);
            var fallback = new StackPanel
            {
                HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center,
                Children = { image, placeholder }
            };
            var preview = new Border
            {
                CornerRadius = new CornerRadius(6), Margin = new Thickness(2), Child = fallback
            };
            preview.SetResourceReference(Border.BackgroundProperty, "SettingsBackground");
            var caption = Text(name, 12, "TextPrimary");
            caption.TextTrimming = TextTrimming.CharacterEllipsis;
            caption.Margin = new Thickness(6, 6, 6, 2);
            var content = new DockPanel { Children = { caption, preview } };
            DockPanel.SetDock(caption, Dock.Bottom);
            var button = ActionButton(content, name, () => Select(window.Handle));
            button.Width = _cardWidth;
            button.Height = _cardHeight;
            button.Margin = new Thickness(Gap / 2);
            button.Padding = new Thickness(5);
            button.HorizontalContentAlignment = HorizontalAlignment.Stretch;
            button.VerticalContentAlignment = VerticalAlignment.Stretch;
            button.GotKeyboardFocus += (_, _) => _selected = window.Handle;
            _cards.Children.Add(button);
            _visible.Add((button, preview, fallback, window));
        }
        _selected = _visible[selected].Window.Handle;
        _previous.IsEnabled = _page > 0;
        _next.IsEnabled = (_page + 1) * _pageSize < _windows.Count;
        _paging.Visibility = _windows.Count > _pageSize ? Visibility.Visible : Visibility.Collapsed;
        _pageLabel.Text = UiText.Format("WindowPickerPage", _page + 1, (_windows.Count - 1) / _pageSize + 1);
        _cards.Rows = (_visible.Count + _columns - 1) / _columns;
        Width = _columns * (_cardWidth + Gap) + FramePadding * 2 + 2;
        Height = _cards.Rows * (_cardHeight + Gap) + FramePadding * 2 + 58
            + (_paging.Visibility == Visibility.Visible ? 43 : 0);
        Position();
        QueueThumbnails();
        if (IsActive) FocusSelection();
    }

    private void HandleKeys(object sender, KeyEventArgs e)
    {
        var index = _visible.FindIndex(card => card.Window.Handle == _selected);
        switch (e.Key)
        {
            case Key.Escape: Close(); break;
            case Key.PageUp: ChangePage(-1); break;
            case Key.PageDown: ChangePage(1); break;
            case Key.Left: FocusCard(index - 1); break;
            case Key.Right: FocusCard(index + 1); break;
            case Key.Up: FocusCard(index - _columns); break;
            case Key.Down: FocusCard(index + _columns); break;
            case Key.Home: FocusCard(0); break;
            case Key.End: FocusCard(_visible.Count - 1); break;
            default: return; // Buttons handle Enter, Space and Tab normally.
        }
        e.Handled = true;
    }

    private void FocusCard(int index)
    {
        if (_visible.Count > 0) _visible[Math.Clamp(index, 0, _visible.Count - 1)].Button.Focus();
    }

    private void FocusSelection() => FocusCard(_visible.FindIndex(card => card.Window.Handle == _selected));

    private void Select(nint handle)
    {
        Close();
        _activate(handle);
    }

    protected override void OnSourceInitialized(EventArgs e)
    {
        base.OnSourceInitialized(e);
        _handle = new WindowInteropHelper(this).Handle;
        MainWindow.ExcludeFromTaskSwitcher(_handle);
        WindowThumbnailPreview.ExcludeFromPeek(_handle);
        Position();
        QueueThumbnails();
    }

    private void Position()
    {
        var area = _display.WorkArea;
        Left = area.Left + (area.Width - Width) / 2;
        Top = area.Top + (area.Height - Height) / 2;
        if (_handle == 0) return;
        var scale = _display.Dpi / 96d;
        SetWindowPos(_handle, new nint(-1), (int)Math.Round(Left * scale), (int)Math.Round(Top * scale),
            (int)Math.Round(Width * scale), (int)Math.Round(Height * scale), 0x0010);
    }

    private void QueueThumbnails()
    {
        if (_thumbnailRefreshQueued) return;
        _thumbnailRefreshQueued = true;
        Dispatcher.BeginInvoke(DispatcherPriority.Loaded, new Action(() =>
        {
            _thumbnailRefreshQueued = false;
            if (_closed || _handle == 0 || !IsVisible || Content is not FrameworkElement root) return;
            ReleaseThumbnails();
            var dpi = VisualTreeHelper.GetDpi(this);
            foreach (var card in _visible)
            {
                var origin = card.Preview.TranslatePoint(new Point(0, 0), root);
                var bounds = new Rect(origin.X * dpi.DpiScaleX, origin.Y * dpi.DpiScaleY,
                    card.Preview.ActualWidth * dpi.DpiScaleX, card.Preview.ActualHeight * dpi.DpiScaleY);
                var thumbnail = WindowThumbnailPreview.RegisterLiveThumbnail(_handle, card.Window.Handle, bounds);
                card.Fallback.Visibility = thumbnail != 0 ? Visibility.Hidden : Visibility.Visible;
                if (thumbnail != 0) _thumbnails.Add(thumbnail);
            }
        }));
    }

    private void ReleaseThumbnails()
    {
        foreach (var thumbnail in _thumbnails) WindowThumbnailPreview.UnregisterLiveThumbnail(thumbnail);
        _thumbnails.Clear();
    }

    protected override void OnClosed(EventArgs e)
    {
        _closed = true;
        ReleaseThumbnails();
        base.OnClosed(e);
    }

    private static TextBlock Text(string text, double size, string color)
    {
        var block = new TextBlock { Text = text, FontSize = size };
        block.SetResourceReference(TextBlock.ForegroundProperty, color);
        return block;
    }

    private static Button ActionButton(object content, string name, Action action)
    {
        var button = new Button
        {
            Content = content, MinWidth = 34, MinHeight = 28, Padding = new Thickness(6, 0, 6, 0),
            Style = PickerButtonStyle, HorizontalContentAlignment = HorizontalAlignment.Center,
            VerticalContentAlignment = VerticalAlignment.Center
        };
        AutomationProperties.SetName(button, name);
        button.ToolTip = name;
        button.SetResourceReference(ForegroundProperty, "TextPrimary");
        button.SetResourceReference(BackgroundProperty, "SettingsControl");
        button.SetResourceReference(BorderBrushProperty, "StrokeSubtle");
        button.Click += (_, _) => action();
        return button;
    }

    private static Style CreateButtonStyle()
    {
        var frame = new FrameworkElementFactory(typeof(Border), "Frame");
        frame.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
        frame.SetValue(Border.BorderBrushProperty, new TemplateBindingExtension(Control.BorderBrushProperty));
        frame.SetValue(Border.BorderThicknessProperty, new Thickness(2));
        frame.SetValue(Border.CornerRadiusProperty, new CornerRadius(9));
        frame.SetValue(Border.PaddingProperty, new TemplateBindingExtension(Control.PaddingProperty));
        var presenter = new FrameworkElementFactory(typeof(ContentPresenter));
        presenter.SetValue(HorizontalAlignmentProperty, new TemplateBindingExtension(Control.HorizontalContentAlignmentProperty));
        presenter.SetValue(VerticalAlignmentProperty, new TemplateBindingExtension(Control.VerticalContentAlignmentProperty));
        frame.AppendChild(presenter);
        var template = new ControlTemplate(typeof(Button)) { VisualTree = frame };
        var hover = new Trigger { Property = IsMouseOverProperty, Value = true };
        hover.Setters.Add(new Setter(Border.BackgroundProperty, new DynamicResourceExtension("SurfaceHover"), "Frame"));
        var focus = new Trigger { Property = IsKeyboardFocusedProperty, Value = true };
        focus.Setters.Add(new Setter(Border.BorderBrushProperty, new DynamicResourceExtension("StrokeFocus"), "Frame"));
        var disabled = new Trigger { Property = IsEnabledProperty, Value = false };
        disabled.Setters.Add(new Setter(OpacityProperty, 0.45));
        template.Triggers.Add(hover);
        template.Triggers.Add(focus);
        template.Triggers.Add(disabled);
        var style = new Style(typeof(Button));
        style.Setters.Add(new Setter(Control.TemplateProperty, template));
        style.Setters.Add(new Setter(FocusVisualStyleProperty, null));
        return style;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint window, nint after, int x, int y, int width, int height, uint flags);
}
