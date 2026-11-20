using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

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
    private readonly Border _preview = new() { Height = 38, CornerRadius = new CornerRadius(5), Margin = new Thickness(0, 12, 0, 10) };
    private readonly Slider[] _channels = new Slider[3];
    private readonly TextBox _hex = new() { Width = 105, Margin = new Thickness(8, 0, 0, 0) };
    private readonly Slider _angle = new() { Minimum = 0, Maximum = 360, Value = 90, Width = 180 };
    private TabControl _tabs = null!;
    private bool _updating;
    private Color _solid;

    public Brush? Result { get; private set; }

    public ThemeBrushPicker(Brush source)
    {
        Title = "Color";
        Width = 390;
        Height = 565;
        MinWidth = 350;
        MinHeight = 480;
        ResizeMode = ResizeMode.CanResize;
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        Background = (Brush)Application.Current.Resources["SettingsBackground"];
        Foreground = (Brush)Application.Current.Resources["TextPrimary"];
        FontFamily = new FontFamily("Segoe UI");
        FontSize = 13;

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
        Content = outer;
        var footer = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0, 15, 0, 0) };
        DockPanel.SetDock(footer, Dock.Bottom);
        outer.Children.Add(footer);
        var cancel = Button("Cancel");
        cancel.Click += (_, _) => { DialogResult = false; Close(); };
        var accept = Button("Use color");
        accept.Margin = new Thickness(8, 0, 0, 0);
        accept.Click += (_, _) => { Result = BuildBrush(); DialogResult = true; Close(); };
        footer.Children.Add(cancel);
        footer.Children.Add(accept);

        var scroll = new ScrollViewer { VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        outer.Children.Add(scroll);
        var body = new StackPanel();
        scroll.Content = body;
        _tabs = new TabControl();
        body.Children.Add(_tabs);

        var solid = new StackPanel { Margin = new Thickness(7) };
        solid.Children.Add(new TextBlock { Text = "Choose a single color.", Margin = new Thickness(0, 8, 0, 8) });
        _tabs.Items.Add(new TabItem { Header = "Solid", Content = solid });

        var grad = new StackPanel { Margin = new Thickness(7) };
        grad.Children.Add(new TextBlock { Text = "Select a stop to edit its color.", Margin = new Thickness(0, 8, 0, 8) });
        _stopList.SelectionChanged += (_, _) => SyncChannels();
        grad.Children.Add(_stopList);
        var tools = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 7, 0, 9) };
        var add = Button("+ Stop");
        add.Click += (_, _) =>
        {
            if (_stops.Count >= 16) return;
            _stops.Add(new Stop(CurrentColor(), .5));
            RefreshStops();
            _stopList.SelectedIndex = _stops.Count - 1;
        };
        var remove = Button("Remove");
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
        position.ValueChanged += (_, _) =>
        {
            if (_updating || _stopList.SelectedIndex < 0) return;
            _stops[_stopList.SelectedIndex].Offset = position.Value / 100;
            RefreshStops(_stopList.SelectedIndex);
            UpdatePreview();
        };
        _stopList.SelectionChanged += (_, _) =>
        {
            if (_stopList.SelectedIndex < 0) return;
            _updating = true;
            position.Value = _stops[_stopList.SelectedIndex].Offset * 100;
            _updating = false;
        };
        grad.Children.Add(new TextBlock { Text = "Stop position", Margin = new Thickness(0, 3, 0, 5) });
        grad.Children.Add(position);
        var angleRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 12, 0, 0) };
        angleRow.Children.Add(new TextBlock { Text = "Angle", Width = 85, VerticalAlignment = VerticalAlignment.Center });
        _angle.ValueChanged += (_, _) => UpdatePreview();
        angleRow.Children.Add(_angle);
        grad.Children.Add(angleRow);
        _tabs.Items.Add(new TabItem { Header = "Gradient", Content = grad });
        _tabs.SelectionChanged += (_, e) => { if (ReferenceEquals(e.Source, _tabs)) { SyncChannels(); UpdatePreview(); } };
        _tabs.SelectedIndex = source is LinearGradientBrush ? 1 : 0;

        body.Children.Add(_preview);
        for (var i = 0; i < 3; i++)
        {
            var index = i;
            var row = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 5, 0, 5) };
            row.Children.Add(new TextBlock { Text = new[] { "Red", "Green", "Blue" }[i], Width = 55,
                VerticalAlignment = VerticalAlignment.Center });
            var slider = new Slider { Minimum = 0, Maximum = 255, Width = 230, TickFrequency = 1,
                IsSnapToTickEnabled = true };
            _channels[i] = slider;
            slider.ValueChanged += (_, _) => SetChannel(index, (byte)Math.Round(slider.Value));
            row.Children.Add(slider);
            body.Children.Add(row);
        }

        var hexRow = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 14, 0, 0) };
        hexRow.Children.Add(new TextBlock { Text = "HEX", Width = 55, VerticalAlignment = VerticalAlignment.Center });
        hexRow.Children.Add(_hex);
        _hex.LostFocus += (_, _) => ParseHex();
        _hex.KeyDown += (_, e) => { if (e.Key == System.Windows.Input.Key.Return) ParseHex(); };
        body.Children.Add(hexRow);
        RefreshStops();
        _stopList.SelectedIndex = 0;
        SyncChannels();
        UpdatePreview();
    }

    private static Button Button(string label) => new()
    {
        Content = label, MinWidth = 72, MinHeight = 31, Padding = new Thickness(10, 4, 10, 4)
    };

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
        _hex.Text = c.ToString(CultureInfo.InvariantCulture);
        _updating = false;
    }

    private void RefreshStops(int selected = -1)
    {
        _stopList.ItemsSource = null;
        _stopList.ItemsSource = _stops.ToArray();
        if (selected >= 0 && selected < _stops.Count) _stopList.SelectedIndex = selected;
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
