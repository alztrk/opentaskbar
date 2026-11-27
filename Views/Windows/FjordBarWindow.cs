using System;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar.Services;

namespace OpenTaskbar;

internal sealed partial class FjordBarWindow : Window
{
    private static readonly Brush Ink = BrushAt("#F2F6FC");
    private static readonly Brush Quiet = BrushAt("#B4C3D8");
    private static readonly Brush Accent = BrushAt("#9AD8DE");
    private static readonly Brush Panel = BrushAt("#F2162336");
    private static readonly Brush Hairline = BrushAt("#8A536983");
    private static readonly Style ActionStyle = BuildButtonStyle();
    private static readonly ControlTemplate WorkspaceMenuTemplate = BuildWorkspaceMenuTemplate();
    private static readonly ControlTemplate WorkspaceItemTemplate = BuildWorkspaceItemTemplate();
    private readonly TextBlock _time = new();
    private readonly Popup _wifiPopup;
    private readonly uint _appBarMessage = FjordBarNative.RegisterWindowMessage("OpenTaskbar.FjordBar.AppBar");
    private readonly uint _taskbarCreated = FjordBarNative.RegisterWindowMessage("TaskbarCreated");
    private DisplayInfo _display;
    private HwndSource? _source;
    private nint _handle;
    private FjordBarNative.Rect? _placement;
    private bool _registered;
    private bool _hiddenForFullscreen;
    private bool _repositionQueued;
    private bool _closed;

    internal FjordBarWindow(DisplayInfo display, Action close, FjordFilesAvailability files)
    {
        _display = display;
        Title = FjordBarText.Get(display.IsPrimary ? "PrimaryBar" : "SecondaryBar");
        WindowStyle = WindowStyle.None;
        ResizeMode = ResizeMode.NoResize;
        ShowInTaskbar = false;
        ShowActivated = false;
        AllowsTransparency = true;
        Background = Brushes.Transparent;
        Topmost = true;
        WindowStartupLocation = WindowStartupLocation.Manual;
        FontFamily = new FontFamily("Segoe UI Variable Text, Segoe UI");
        UseLayoutRounding = true;
        SnapsToDevicePixels = true;
        Left = display.Bounds.Left;
        Top = display.Bounds.Top;
        Width = display.Bounds.Width;
        Height = 32;

        var layout = new Grid { Margin = new Thickness(12, 0, 12, 0) };
        layout.ColumnDefinitions.Add(new ColumnDefinition());
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var left = new DockPanel { LastChildFill = true, VerticalAlignment = VerticalAlignment.Center };
        var workspace = ButtonFor(Text("◆", 15, Accent, FontWeights.SemiBold), FjordBarText.Get("BarMenu"));
        var workspaceMenu = new ContextMenu
        {
            PlacementTarget = workspace, Placement = PlacementMode.Bottom,
            Background = Panel, Foreground = Ink, BorderBrush = Hairline, BorderThickness = new Thickness(1),
            Template = WorkspaceMenuTemplate
        };
        var filesMenuItem = AddMenu(workspaceMenu, FjordBarText.Get("Files"), () =>
        {
            if (files.Refresh() && !FjordFilesActivation.TrySend(null)) FjordFilesWindow.Open(null, _display);
        });
        void UpdateFilesMenu(bool enabled) => filesMenuItem.Visibility = enabled ? Visibility.Visible : Visibility.Collapsed;
        UpdateFilesMenu(files.Enabled);
        workspaceMenu.Opened += (_, _) => UpdateFilesMenu(files.Refresh());
        files.Changed += UpdateFilesMenu;
        Closed += (_, _) => files.Changed -= UpdateFilesMenu;
        AddMenu(workspaceMenu, FjordBarText.Get("CloseBar"), close);
        workspace.ContextMenu = workspaceMenu;
        workspace.Click += (_, _) => workspaceMenu.IsOpen = true;
        left.Children.Add(workspace);
        layout.Children.Add(left);

        var right = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var wifi = ButtonFor(Glyph("\uE701"), FjordBarText.Get("WifiNetworks"));
        _wifiPopup = CreateWifiPopup(wifi);
        right.Children.Add(wifi);
        SetText(_time, 12.5, Ink, FontWeights.SemiBold);
        _time.Margin = new Thickness(10, 0, 2, 0);
        right.Children.Add(_time);
        Grid.SetColumn(right, 1);
        layout.Children.Add(right);
        Content = new Border
        {
            Background = Panel, BorderBrush = Hairline, BorderThickness = new Thickness(0, 0, 0, 1),
            Child = layout
        };
        Update(display, false);
    }

    protected override void OnSourceInitialized(EventArgs e)
    {
        base.OnSourceInitialized(e);
        _handle = new WindowInteropHelper(this).Handle;
        _source = HwndSource.FromHwnd(_handle);
        _source?.AddHook(WindowProc);
        var style = FjordBarNative.GetWindowLongPtr(_handle, -20).ToInt64();
        FjordBarNative.SetWindowLongPtr(_handle, -20, new nint(style | 0x80 | 0x08000000)); // TOOLWINDOW | NOACTIVATE
        Register();
        Position();
    }

    private void Register()
    {
        var data = CreateData();
        data.Callback = _appBarMessage;
        _registered = FjordBarNative.SHAppBarMessage(FjordBarNative.AppBarNew, ref data) != 0;
        if (!_registered) AppLog.Warning("Windows rejected Fjord's AppBar registration.");
    }

    private FjordBarNative.AppBarData CreateData() => new()
    {
        Size = (uint)Marshal.SizeOf<FjordBarNative.AppBarData>(), Window = _handle, Edge = FjordBarNative.TopEdge
    };

    private void Position()
    {
        if (_handle == 0 || _closed) return;
        var expected = FjordBarNative.PixelBounds(_display);
        var height = (int)Math.Ceiling(32d * _display.Dpi / 96);
        expected.Bottom = expected.Top + height;
        var data = CreateData();
        data.Bounds = expected;
        if (_registered)
        {
            FjordBarNative.SHAppBarMessage(FjordBarNative.AppBarQueryPosition, ref data);
            data.Bounds.Bottom = data.Bounds.Top + height;
        }
        if (_placement is { } previous && previous.Equals(data.Bounds)) return;
        if (_registered && FjordBarNative.SHAppBarMessage(FjordBarNative.AppBarSetPosition, ref data) == 0)
            AppLog.Warning("Windows rejected Fjord's AppBar position.");
        _placement = data.Bounds;
        var bar = data.Bounds;
        FjordBarNative.SetWindowPos(_handle, new nint(-1), bar.Left, bar.Top, bar.Width, bar.Height, FjordBarNative.NoActivate | FjordBarNative.NoOwnerZOrder);
    }

    internal void Update(DisplayInfo display, bool fullscreen)
    {
        var geometryChanged = !_display.Bounds.Equals(display.Bounds) || _display.Dpi != display.Dpi;
        _display = display;
        if (geometryChanged) { _placement = null; Position(); }
        _time.Text = DateTime.Now.ToString("t", FjordBarText.Culture);

        if (_handle == 0 || fullscreen == _hiddenForFullscreen) return;
        _hiddenForFullscreen = fullscreen;
        if (fullscreen) _wifiPopup.IsOpen = false;
        FjordBarNative.ShowWindow(_handle, fullscreen ? 0 : 4);
        if (!fullscreen) { _placement = null; Position(); }
    }

    private nint WindowProc(nint window, int message, nint wParam, nint lParam, ref bool handled)
    {
        if ((uint)message == _taskbarCreated)
        {
            _registered = false;
            _placement = null;
            QueuePosition();
        }
        else if ((uint)message == _appBarMessage && wParam == 1) QueuePosition();
        else if (message is 0x007E or 0x02E0) QueuePosition();
        return 0;
    }

    private void QueuePosition()
    {
        if (_repositionQueued || _closed) return;
        _repositionQueued = true;
        Dispatcher.BeginInvoke(DispatcherPriority.Background, new Action(() =>
        {
            _repositionQueued = false;
            if (_closed) return;
            if (!_registered) Register();
            Position();
        }));
    }

    protected override void OnClosed(EventArgs e)
    {
        _closed = true;
        _wifiPopup.IsOpen = false;
        if (_registered)
        {
            var data = CreateData();
            FjordBarNative.SHAppBarMessage(FjordBarNative.AppBarRemove, ref data);
            _registered = false;
        }
        _source?.RemoveHook(WindowProc);
        base.OnClosed(e);
    }

    private static MenuItem AddMenu(ContextMenu menu, string title, Action action)
    {
        var item = new MenuItem { Header = title, Foreground = Ink, Template = WorkspaceItemTemplate };
        item.Click += (_, _) => action();
        menu.Items.Add(item);
        return item;
    }

    private static ControlTemplate BuildWorkspaceMenuTemplate()
    {
        var template = new ControlTemplate(typeof(ContextMenu));
        var surface = new FrameworkElementFactory(typeof(Border));
        surface.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
        surface.SetValue(Border.BorderBrushProperty, new TemplateBindingExtension(Control.BorderBrushProperty));
        surface.SetValue(Border.BorderThicknessProperty, new TemplateBindingExtension(Control.BorderThicknessProperty));
        surface.SetValue(Border.PaddingProperty, new Thickness(3));
        surface.SetValue(FrameworkElement.MinWidthProperty, 192d);
        surface.AppendChild(new FrameworkElementFactory(typeof(ItemsPresenter)));
        template.VisualTree = surface;
        return template;
    }

    private static ControlTemplate BuildWorkspaceItemTemplate()
    {
        var template = new ControlTemplate(typeof(MenuItem));
        var surface = new FrameworkElementFactory(typeof(Border), "Surface");
        surface.SetValue(Border.BackgroundProperty, Brushes.Transparent);
        surface.SetValue(Border.PaddingProperty, new Thickness(9, 3, 9, 3));
        var header = new FrameworkElementFactory(typeof(ContentPresenter));
        header.SetValue(ContentPresenter.ContentSourceProperty, "Header");
        header.SetValue(ContentPresenter.RecognizesAccessKeyProperty, true);
        surface.AppendChild(header);
        template.VisualTree = surface;
        var highlighted = new Trigger { Property = MenuItem.IsHighlightedProperty, Value = true };
        highlighted.Setters.Add(new Setter(Border.BackgroundProperty, BrushAt("#405C7598"), "Surface"));
        template.Triggers.Add(highlighted);
        return template;
    }

    private static Button ButtonFor(object content, string name)
    {
        var button = new Button { Content = content, ToolTip = name, Style = ActionStyle, Cursor = Cursors.Hand };
        AutomationProperties.SetName(button, name);
        return button;
    }
    private static void SetText(TextBlock text, double size, Brush color, FontWeight? weight = null)
    {
        text.FontSize = size;
        text.Foreground = color;
        text.FontWeight = weight ?? FontWeights.Normal;
        text.VerticalAlignment = VerticalAlignment.Center;
    }
    private static TextBlock Text(string value, double size, Brush color, FontWeight? weight = null)
    {
        var text = new TextBlock { Text = value };
        SetText(text, size, color, weight);
        return text;
    }
    private static TextBlock Glyph(string value)
    {
        var glyph = Text(value, 14, Quiet);
        glyph.FontFamily = new FontFamily("Segoe Fluent Icons");
        return glyph;
    }
    private static Brush BrushAt(string hex)
    {
        var brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex));
        brush.Freeze();
        return brush;
    }
    private static Style BuildButtonStyle()
    {
        var style = new Style(typeof(Button));
        style.Setters.Add(new Setter(Control.BackgroundProperty, Brushes.Transparent));
        style.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(8, 1, 8, 1)));
        style.Setters.Add(new Setter(FrameworkElement.HeightProperty, 25d));
        var template = new ControlTemplate(typeof(Button));
        var border = new FrameworkElementFactory(typeof(Border), "Surface");
        border.SetValue(Border.CornerRadiusProperty, new CornerRadius(5));
        border.SetValue(Border.BackgroundProperty, new TemplateBindingExtension(Control.BackgroundProperty));
        border.SetValue(Border.PaddingProperty, new TemplateBindingExtension(Control.PaddingProperty));
        var presenter = new FrameworkElementFactory(typeof(ContentPresenter));
        presenter.SetValue(VerticalAlignmentProperty, VerticalAlignment.Center);
        presenter.SetValue(HorizontalAlignmentProperty, HorizontalAlignment.Center);
        border.AppendChild(presenter);
        template.VisualTree = border;
        var hover = new Trigger { Property = IsMouseOverProperty, Value = true };
        hover.Setters.Add(new Setter(Border.BackgroundProperty, BrushAt("#405C7598"), "Surface"));
        template.Triggers.Add(hover);
        var focus = new Trigger { Property = IsKeyboardFocusedProperty, Value = true };
        focus.Setters.Add(new Setter(Border.BorderThicknessProperty, new Thickness(1), "Surface"));
        focus.Setters.Add(new Setter(Border.BorderBrushProperty, Accent, "Surface"));
        template.Triggers.Add(focus);
        var disabled = new Trigger { Property = IsEnabledProperty, Value = false };
        disabled.Setters.Add(new Setter(OpacityProperty, 0.5));
        template.Triggers.Add(disabled);
        style.Setters.Add(new Setter(TemplateProperty, template));
        return style;
    }
}
