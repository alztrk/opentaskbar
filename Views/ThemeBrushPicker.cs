using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;
using System.Windows.Input;
using System.Windows.Shapes;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views;

/// <summary>In-app solid/gradient color editor; no operating-system color dialog.</summary>
public sealed class ThemeBrushPicker : Window
{
    private sealed class Stop(Color color, double offset)
    {
        public Color Color { get; set; } = color;
        public double Offset { get; set; } = offset;
        public override string ToString() => $"{Color} · {Offset * 100:0}%";
    }

    private readonly List<Stop> _stops = [];
    private readonly ListBox _stopList = new() { MinHeight = 82, MaxHeight = 130 };
    private readonly StackPanel _stopChoices = new();
    private readonly Border _preview = new() { Height = 38, CornerRadius = new CornerRadius(5), Margin = new Thickness(0, 12, 0, 10) };
    private readonly Slider[] _channels = new Slider[3];
    private readonly Grid _colorField = new() { Width = 305, Height = 126, ClipToBounds = true };
    private readonly Canvas _colorCursorLayer = new() { IsHitTestVisible = false };
    private readonly Ellipse _colorCursor = new()
    {
        Width = 12, Height = 12, Stroke = Brushes.White, StrokeThickness = 2,
        Fill = Brushes.Transparent, IsHitTestVisible = false
    };
    private readonly Slider _hueSlider = new()
    {
        Minimum = 0, Maximum = 360, Width = 305,
        TickFrequency = 1, IsSnapToTickEnabled = true
    };
    private readonly Slider _alphaSlider = new() { Minimum = 0, Maximum = 255, TickFrequency = 1, IsSnapToTickEnabled = true };
    private readonly TextBlock _alphaValue = new() { Width = 42, TextAlignment = TextAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
    private double _hue, _sat, _value;

    private readonly TextBox _hex = new() { Width = 105, Margin = new Thickness(8, 0, 0, 0) };
    private readonly Slider _angle = new() { Minimum = 0, Maximum = 360, Value = 90, Width = 180 };
    private readonly TextBlock _angleValue = new() { Width = 42, TextAlignment = TextAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
    private TabControl _tabs = null!;
    private bool _updating;
    private Color _solid;

    public Brush? Result { get; private set; }

    public ThemeBrushPicker(Brush source)
    {
        Title = UiText.Get("ColorTitle");
        Width = 398;
        Height = 655;
        MinWidth = 350;
        MinHeight = 480;
        ResizeMode = ResizeMode.CanResize;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Background = (Brush)Application.Current.Resources["SettingsBackground"];
        Foreground = (Brush)Application.Current.Resources["TextPrimary"];
        SetResourceReference(Control.FontFamilyProperty, "InterfaceFont");
        FontSize = 13;
        _hueSlider.SetResourceReference(FrameworkElement.StyleProperty, "SettingsOpacitySlider");
        _angle.SetResourceReference(FrameworkElement.StyleProperty, "SettingsOpacitySlider");
        _stopList.SetResourceReference(Control.BackgroundProperty, "SettingsControl");
        _stopList.SetResourceReference(Control.ForegroundProperty, "TextPrimary");
        _stopList.SetResourceReference(Control.BorderBrushProperty, "StrokeSubtle");
        var stopItemStyle = new Style(typeof(ListBoxItem));
        stopItemStyle.Setters.Add(new Setter(Control.ForegroundProperty, new DynamicResourceExtension("TextPrimary")));
        stopItemStyle.Setters.Add(new Setter(Control.BackgroundProperty, Brushes.Transparent));
        stopItemStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(4, 2, 4, 2)));
        var selectedStop = new Trigger { Property = ListBoxItem.IsSelectedProperty, Value = true };
        selectedStop.Setters.Add(new Setter(Control.BackgroundProperty, new DynamicResourceExtension("SurfaceSelected")));
        stopItemStyle.Triggers.Add(selectedStop);
        _stopList.ItemContainerStyle = stopItemStyle;

        _solid = source is SolidColorBrush single ? single.Color : Colors.SlateGray;
        if (source is LinearGradientBrush gradient)
        {
            foreach (var stop in gradient.GradientStops.OrderBy(s => s.Offset))
                _stops.Add(new Stop(stop.Color, stop.Offset));
            var delta = gradient.EndPoint - gradient.StartPoint;
            _angle.Value = (Math.Atan2(delta.X, -delta.Y) * 180 / Math.PI + 360) % 360;
        }
        if (_stops.Count < 2)
        {
            _stops.Clear();
            _stops.Add(new Stop(_solid, 0));
            _stops.Add(new Stop(Color.FromRgb(120, 145, 175), 1));
        }

        var outer = new DockPanel { Margin = new Thickness(18) };
        outer.SetResourceReference(TextElement.ForegroundProperty, "TextPrimary");
        outer.SetResourceReference(TextElement.FontFamilyProperty, "InterfaceFont");
        outer.SetResourceReference(Panel.BackgroundProperty, "SettingsBackground");
        Content = outer;
        var footer = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0, 15, 0, 0) };
        DockPanel.SetDock(footer, Dock.Bottom);
        outer.Children.Add(footer);
        var cancel = Button(UiText.Get("ColorCancel"));
        cancel.Click += (_, _) => { DialogResult = false; Close(); };
        var accept = Button(UiText.Get("ColorUse"));
        accept.Margin = new Thickness(8, 0, 0, 0);
        accept.Click += (_, _) => { Result = BuildBrush(); DialogResult = true; Close(); };
        footer.Children.Add(cancel);
        footer.Children.Add(accept);

        var scroll = new ScrollViewer { VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        outer.Children.Add(scroll);
        var body = new StackPanel();
        scroll.Content = body;
        _tabs = new TabControl();
        _tabs.SetResourceReference(Control.BackgroundProperty, "SettingsBackground");
        _tabs.SetResourceReference(Control.ForegroundProperty, "TextPrimary");
        _tabs.SetResourceReference(Control.BorderBrushProperty, "StrokeSubtle");
        body.Children.Add(_tabs);

        var solid = new StackPanel { Margin = new Thickness(7) };
        solid.Children.Add(new TextBlock { Text = UiText.Get("ColorSelectSingle"), Margin = new Thickness(0, 8, 0, 8) });
        _tabs.Items.Add(CreateTab("Solid", solid));

        var grad = new StackPanel { Margin = new Thickness(7) };
        grad.Children.Add(new TextBlock { Text = UiText.Get("ColorSelectStop"), Margin = new Thickness(0, 8, 0, 8) });
        _stopList.SelectionChanged += (_, _) => SyncChannels();
        grad.Children.Add(_stopChoices);
        var tools = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 7, 0, 9) };
        var add = Button(UiText.Get("ColorAddStop"));
        add.Click += (_, _) =>
        {
            if (_stops.Count >= 16) return;
            _stops.Add(new Stop(CurrentColor(), .5));
            RefreshStops();
            _stopList.SelectedIndex = _stops.Count - 1;
        };
        var remove = Button(UiText.Get("ColorRemove"));
        remove.Margin = new Thickness(8, 0, 0, 0);
        remove.Click += (_, _) =>
        {
            if (_stops.Count <= 2 || _stopList.SelectedIndex < 0) return;
            _stops.RemoveAt(_stopList.SelectedIndex);
            RefreshStops();
            _stopList.SelectedIndex = 0;
        };
        tools.Children.Add(add);
        tools.Children.Add(remove);
        grad.Children.Add(tools);
        var position = new Slider { Minimum = 0, Maximum = 100, TickFrequency = 1, IsSnapToTickEnabled = true };
        position.SetResourceReference(FrameworkElement.StyleProperty, "SettingsOpacitySlider");
        var positionValue = new TextBlock { Width = 42, TextAlignment = TextAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
        positionValue.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        position.ValueChanged += (_, _) =>
        {
            if (_updating || _stopList.SelectedIndex < 0) return;
            _stops[_stopList.SelectedIndex].Offset = position.Value / 100;
            positionValue.Text = Math.Round(position.Value).ToString(CultureInfo.InvariantCulture) + "%";
            RefreshStops(_stopList.SelectedIndex);
            UpdatePreview();
        };
        _stopList.SelectionChanged += (_, _) =>
        {
            UpdateStopChoices();
            if (_stopList.SelectedIndex < 0) return;
            _updating = true;
            position.Value = _stops[_stopList.SelectedIndex].Offset * 100;
            positionValue.Text = Math.Round(position.Value).ToString(CultureInfo.InvariantCulture) + "%";
            _updating = false;
        };
        grad.Children.Add(new TextBlock { Text = UiText.Get("ColorStopPosition"), Margin = new Thickness(0, 3, 0, 5) });
        var positionRow = new DockPanel();
        DockPanel.SetDock(positionValue, Dock.Right);
        positionRow.Children.Add(positionValue);
        positionRow.Children.Add(position);
        grad.Children.Add(positionRow);
        var angleRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 12, 0, 0) };
        angleRow.Children.Add(new TextBlock { Text = UiText.Get("ColorAngle"), Width = 85, VerticalAlignment = VerticalAlignment.Center });
        _angle.ValueChanged += (_, _) => UpdatePreview();
        angleRow.Children.Add(_angle);
        _angleValue.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        _angleValue.Text = Math.Round(_angle.Value).ToString(CultureInfo.InvariantCulture) + "°";
        _angle.ValueChanged += (_, _) => _angleValue.Text = Math.Round(_angle.Value).ToString(CultureInfo.InvariantCulture) + "°";
        angleRow.Children.Add(_angleValue);
        grad.Children.Add(angleRow);
        _tabs.Items.Add(CreateTab("Gradient", grad));
        _tabs.SelectionChanged += (_, e) => { if (ReferenceEquals(e.Source, _tabs)) { SyncChannels(); UpdatePreview(); } };
        _tabs.SelectedIndex = source is LinearGradientBrush ? 1 : 0;

        var colorArea = new StackPanel { Margin = new Thickness(6, 12, 0, 8) };
        colorArea.Children.Add(new TextBlock
        {
            Text = UiText.Get("ColorTitle"), FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(0, 0, 0, 9)
        });
        _colorField.Children.Add(new Rectangle
        {
            Fill = new LinearGradientBrush(Colors.White, Color.FromArgb(0, 255, 255, 255),
                new Point(0, 0), new Point(1, 0))
        });
        _colorField.Children.Add(new Rectangle
        {
            Fill = new LinearGradientBrush(Color.FromArgb(0, 0, 0, 0), Colors.Black,
                new Point(0, 0), new Point(0, 1))
        });
        _colorCursorLayer.Children.Add(_colorCursor);
        _colorField.Children.Add(_colorCursorLayer);
        _colorField.MouseLeftButtonDown += (_, e) =>
        {
            _colorField.CaptureMouse();
            ChooseColorAt(e.GetPosition(_colorField));
        };
        _colorField.MouseMove += (_, e) =>
        {
            if (e.LeftButton == MouseButtonState.Pressed && _colorField.IsMouseCaptured)
                ChooseColorAt(e.GetPosition(_colorField));
        };
        _colorField.MouseLeftButtonUp += (_, _) => _colorField.ReleaseMouseCapture();
        colorArea.Children.Add(_colorField);

        var rainbow = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(1, 0) };
        rainbow.GradientStops.Add(new GradientStop(Colors.Red, 0));
        rainbow.GradientStops.Add(new GradientStop(Colors.Yellow, 1d / 6));
        rainbow.GradientStops.Add(new GradientStop(Colors.Lime, 2d / 6));
        rainbow.GradientStops.Add(new GradientStop(Colors.Cyan, 3d / 6));
        rainbow.GradientStops.Add(new GradientStop(Colors.Blue, 4d / 6));
        rainbow.GradientStops.Add(new GradientStop(Colors.Magenta, 5d / 6));
        rainbow.GradientStops.Add(new GradientStop(Colors.Red, 1));
        colorArea.Children.Add(new Border
        {
            Height = 8, Width = 305, Background = rainbow,
            CornerRadius = new CornerRadius(4), Margin = new Thickness(0, 12, 0, 0),
            HorizontalAlignment = HorizontalAlignment.Left
        });
        _hueSlider.ValueChanged += (_, _) =>
        {
            if (_updating) return;
            _hue = _hueSlider.Value;
            SetColor(ColorFromHsv(_hue, _sat, _value, CurrentColor().A));
        };
        colorArea.Children.Add(_hueSlider);
        body.Children.Add(colorArea);
        body.Children.Add(_preview);
        for (var i = 0; i < 3; i++)
        {
            var index = i;
            var row = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 5, 0, 5) };
            row.Children.Add(new TextBlock { Text = new[] { UiText.Get("ColorRed"), UiText.Get("ColorGreen"), UiText.Get("ColorBlue") }[i], Width = 55,
                VerticalAlignment = VerticalAlignment.Center });
            var slider = new Slider { Minimum = 0, Maximum = 255, Width = 230, TickFrequency = 1,
                IsSnapToTickEnabled = true };
            slider.SetResourceReference(FrameworkElement.StyleProperty, "SettingsOpacitySlider");
            _channels[i] = slider;
            slider.ValueChanged += (_, _) => SetChannel(index, (byte)Math.Round(slider.Value));
            row.Children.Add(slider);
            body.Children.Add(row);
        }

        var alphaRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 8, 0, 0) };
        alphaRow.Children.Add(new TextBlock { Text = UiText.Get("ColorOpacity"), Width = 55, VerticalAlignment = VerticalAlignment.Center });
        _alphaSlider.Width = 200;
        _alphaSlider.VerticalAlignment = VerticalAlignment.Center;
        AutomationProperties.SetName(_alphaSlider, "Color opacity");
        _alphaSlider.SetResourceReference(FrameworkElement.StyleProperty, "SettingsOpacitySlider");
        _alphaSlider.ValueChanged += (_, _) =>
        {
            if (!_updating)
            {
                var current = CurrentColor();
                SetColor(Color.FromArgb((byte)Math.Round(_alphaSlider.Value), current.R, current.G, current.B));
            }
        };
        alphaRow.Children.Add(_alphaSlider);
        alphaRow.Children.Add(_alphaValue);
        body.Children.Add(alphaRow);

        var hexRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 14, 0, 0) };
        hexRow.Children.Add(new TextBlock { Text = UiText.Get("ColorHEX"), Width = 55, VerticalAlignment = VerticalAlignment.Center });
        hexRow.Children.Add(_hex);
        _hex.LostFocus += (_, _) => ParseHex();
        _hex.KeyDown += (_, e) => { if (e.Key == System.Windows.Input.Key.Return) ParseHex(); };
        _hex.SetResourceReference(Control.BackgroundProperty, "SettingsControl");
        _hex.SetResourceReference(Control.ForegroundProperty, "TextPrimary");
        _hex.SetResourceReference(Control.BorderBrushProperty, "StrokeSubtle");
        body.Children.Add(hexRow);
        RefreshStops();
        _stopList.SelectedIndex = 0;
        SyncChannels();
        UpdatePreview();
    }

    private static Button Button(string label) => new()
    {
        Content = label, MinWidth = 72, MinHeight = 31, Padding = new Thickness(10, 4, 10, 4),
        Style = (Style)Application.Current.FindResource("SettingsButton")
    };

    private static TabItem CreateTab(string header, UIElement content)
    {
        var tab = new TabItem { Header = header, Content = content, Template = CreateTabTemplate() };
        tab.SetResourceReference(Control.ForegroundProperty, "TextPrimary");
        tab.SetResourceReference(Control.BackgroundProperty, "SettingsControl");
        tab.SetResourceReference(Control.BorderBrushProperty, "StrokeSubtle");
        tab.Padding = new Thickness(9, 4, 9, 4);
        return tab;
    }

    private static ControlTemplate CreateTabTemplate()
    {
        var template = new ControlTemplate(typeof(TabItem));
        var border = new FrameworkElementFactory(typeof(Border)) { Name = "TabBorder" };
        border.SetValue(Border.BackgroundProperty, new DynamicResourceExtension("SettingsControl"));
        border.SetValue(Border.BorderBrushProperty, new DynamicResourceExtension("StrokeSubtle"));
        border.SetValue(Border.BorderThicknessProperty, new Thickness(1));
        var header = new FrameworkElementFactory(typeof(ContentPresenter));
        header.SetValue(ContentPresenter.ContentSourceProperty, "Header");
        header.SetValue(ContentPresenter.RecognizesAccessKeyProperty, true);
        header.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
        header.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
        header.SetValue(FrameworkElement.MarginProperty, new Thickness(9, 4, 9, 4));
        border.AppendChild(header);
        template.VisualTree = border;
        var selected = new Trigger { Property = TabItem.IsSelectedProperty, Value = true };
        selected.Setters.Add(new Setter(Border.BackgroundProperty,
            new DynamicResourceExtension("SurfaceSelected"), "TabBorder"));
        template.Triggers.Add(selected);
        return template;
    }

    private Color CurrentColor() => _tabs.SelectedIndex == 1 && _stopList.SelectedIndex >= 0
        ? _stops[_stopList.SelectedIndex].Color : _solid;

    private void SetChannel(int channel, byte value)
    {
        if (_updating) return;
        var c = CurrentColor();
        var channels = new[] { c.R, c.G, c.B };
        channels[channel] = value;
        SetColor(Color.FromArgb(c.A, channels[0], channels[1], channels[2]));
    }

    private void SetColor(Color value)
    {
        if (_tabs.SelectedIndex == 1 && _stopList.SelectedIndex >= 0)
            _stops[_stopList.SelectedIndex].Color = value;
        else
            _solid = value;
        SyncChannels();
        RefreshStops(_stopList.SelectedIndex);
        UpdatePreview();
    }

    private void ParseHex()
    {
        try
        {
            var text = _hex.Text.Trim();
            if (text.Length is 7 or 9 && text.StartsWith('#'))
                SetColor((Color)ColorConverter.ConvertFromString(text));
            else SyncChannels();
        }
        catch (FormatException) { SyncChannels(); }
    }

    private void SyncChannels()
    {
        if (_channels.Any(s => s is null)) return;
        _updating = true;
        var c = CurrentColor();
        _channels[0].Value = c.R;
        _channels[1].Value = c.G;
        _channels[2].Value = c.B;
        _alphaSlider.Value = c.A;
        _alphaValue.Text = Math.Round(c.A / 255d * 100).ToString(CultureInfo.InvariantCulture) + "%";
        _hex.Text = c.ToString(CultureInfo.InvariantCulture);
        _updating = false;
        RefreshColorField();
    }

    private void RefreshStops(int selected = -1)
    {
        _stopList.ItemsSource = null;
        _stopList.Items.Clear();
        foreach (var stop in _stops)
        {
            var label = new TextBlock { Text = $"{stop.Color} · {stop.Offset * 100:0}%", Margin = new Thickness(6, 3, 6, 3) };
            label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            _stopList.Items.Add(new ListBoxItem { Content = label, HorizontalContentAlignment = HorizontalAlignment.Stretch });
        }
        if (selected >= 0 && selected < _stops.Count) _stopList.SelectedIndex = selected;
        UpdateStopChoices();
    }

    private void UpdateStopChoices()
    {
        _stopChoices.Children.Clear();
        for (var index = 0; index < _stops.Count; index++)
        {
            var stop = _stops[index];
            var button = Button($"Stop {index + 1} · {stop.Color} · {stop.Offset * 100:0}%");
            button.HorizontalAlignment = HorizontalAlignment.Stretch;
            button.HorizontalContentAlignment = HorizontalAlignment.Left;
            button.Margin = new Thickness(0, 0, 0, 5);
            if (index == _stopList.SelectedIndex)
                button.SetResourceReference(Control.BackgroundProperty, "SurfaceSelectedStrong");
            var selectedIndex = index;
            button.Click += (_, _) => _stopList.SelectedIndex = selectedIndex;
            _stopChoices.Children.Add(button);
        }
    }

    private void ChooseColorAt(Point position)
    {
        _sat = Math.Clamp(position.X / _colorField.Width, 0, 1);
        _value = Math.Clamp(1 - position.Y / _colorField.Height, 0, 1);
        SetColor(ColorFromHsv(_hue, _sat, _value, CurrentColor().A));
    }

    private void RefreshColorField()
    {
        var color = CurrentColor();
        var (hue, saturation, brightness) = ColorToHsv(color);
        // Preserve the selected hue while editing achromatic colors (white, grey, black).
        var effectiveHue = saturation == 0 || brightness == 0 ? _hue : hue;
        _hue = effectiveHue;
        _sat = saturation;
        _value = brightness;
        _updating = true;
        _hueSlider.Value = effectiveHue;
        _colorField.Background = new SolidColorBrush(ColorFromHsv(effectiveHue, 1, 1));
        Canvas.SetLeft(_colorCursor, Math.Clamp(saturation * _colorField.Width - 6, -6, _colorField.Width - 6));
        Canvas.SetTop(_colorCursor, Math.Clamp((1 - brightness) * _colorField.Height - 6, -6, _colorField.Height - 6));
        _updating = false;
    }

    private static (double Hue, double Saturation, double Brightness) ColorToHsv(Color color)
    {
        var red = color.R / 255d;
        var green = color.G / 255d;
        var blue = color.B / 255d;
        var max = Math.Max(red, Math.Max(green, blue));
        var min = Math.Min(red, Math.Min(green, blue));
        var diff = max - min;
        var hue = 0d;
        if (diff > 0)
        {
            if (max == red) hue = 60 * ((green - blue) / diff % 6);
            else if (max == green) hue = 60 * ((blue - red) / diff + 2);
            else hue = 60 * ((red - green) / diff + 4);
        }
        if (hue < 0) hue += 360;
        return (hue, max <= 0 ? 0 : diff / max, max);
    }

    private static Color ColorFromHsv(double hue, double saturation, double brightness, byte alpha = 255)
    {
        hue = (hue % 360 + 360) % 360;
        saturation = Math.Clamp(saturation, 0, 1);
        brightness = Math.Clamp(brightness, 0, 1);
        var chroma = brightness * saturation;
        var x = chroma * (1 - Math.Abs((hue / 60) % 2 - 1));
        var shift = brightness - chroma;
        var (red, green, blue) = (hue / 60) switch
        {
            < 1 => (chroma, x, 0d),
            < 2 => (x, chroma, 0d),
            < 3 => (0d, chroma, x),
            < 4 => (0d, x, chroma),
            < 5 => (x, 0d, chroma),
            _ => (chroma, 0d, x)
        };
        return Color.FromArgb(alpha,
            (byte)Math.Round((red + shift) * 255),
            (byte)Math.Round((green + shift) * 255),
            (byte)Math.Round((blue + shift) * 255));
    }

    private Brush BuildBrush()
    {
        if (_tabs.SelectedIndex != 1) return new SolidColorBrush(_solid);
        var radians = _angle.Value * Math.PI / 180;
        var gradient = new LinearGradientBrush
        {
            StartPoint = new Point(.5 - Math.Sin(radians) / 2, .5 + Math.Cos(radians) / 2),
            EndPoint = new Point(.5 + Math.Sin(radians) / 2, .5 - Math.Cos(radians) / 2)
        };
        foreach (var stop in _stops.OrderBy(s => s.Offset))
            gradient.GradientStops.Add(new GradientStop(stop.Color, stop.Offset));
        return gradient;
    }

    private void UpdatePreview()
    {
        if (_tabs is null) return;
        _preview.Background = BuildBrush();
    }
}
