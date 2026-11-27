using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Shows the active keyboard input language and lets the user switch between installed Windows layouts (#156).
/// </summary>
public sealed class KeyboardLayoutWidget : DockWidget
{
    private readonly DockFlyout _flyout = new();
    private readonly Button _button;
    private readonly TextBlock _codeText;
    private readonly ContentControl _icon;
    private KeyboardLayoutService.KeyboardLayoutInfo _active;
    private nint _lastTargetWindow;
    private string _lastRenderKey = string.Empty;

    internal Func<IReadOnlyList<KeyboardLayoutService.KeyboardLayoutInfo>> ReadInstalledLayouts { get; init; } =
        KeyboardLayoutService.GetInstalledLayouts;
    internal Func<nint, KeyboardLayoutService.KeyboardLayoutInfo> ReadActiveLayout { get; init; } =
        KeyboardLayoutService.GetActiveLayout;
    internal Func<nint, nint, bool> ApplyLayout { get; init; } =
        KeyboardLayoutService.SwitchLayout;

    public override bool SupportsCompactMode => true;
    public override bool SupportsStaticPreview => true;
    public override int ShrinkPriority => 75;
    public override double FullWidthEstimate => 58;
    public override double CompactWidthEstimate => 42;

    public override UIElement BuildDynamicIslandExpandedContent() => BuildFlyoutContent();

    public KeyboardLayoutWidget()
    {
        _active = KeyboardLayoutService.GetActiveLayout();

        _icon = new ContentControl
        {
            Width = 13,
            Height = 13,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 5, 0)
        };
        _icon.SetResourceReference(StyleProperty, "WidgetIcon");
        _icon.SetResourceReference(ContentControl.ContentProperty, "IconKeyboard");
        _icon.SetResourceReference(ForegroundProperty, "TextSecondary");

        _codeText = new TextBlock
        {
            Text = _active.ShortCode,
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center
        };
        _codeText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

        var content = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center
        };
        content.Children.Add(_icon);
        content.Children.Add(_codeText);

        _button = new Button
        {
            Content = content,
            Padding = new Thickness(8, 0, 8, 0)
        };
        _button.SetResourceReference(StyleProperty, "WidgetCardButton");
        _button.Click += Button_Click;
        _button.PreviewMouseWheel += Button_PreviewMouseWheel;

        Content = _button;
        ApplyDensityLayout();
        Render();
    }

    protected override void OnRenderPreview()
    {
        _active = ReadActiveLayout(0);
        Render();
    }

    protected override void OnActivated()
    {
        _lastRenderKey = string.Empty;
        CaptureTargetWindow();
        RefreshActiveLayout();
    }

    protected override void OnDeactivated() => _flyout.Close();

    protected override void OnTick(DateTime now)
    {
        CaptureTargetWindow();
        RefreshActiveLayout();
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();
    protected override void OnLanguageChanged()
    {
        _lastRenderKey = string.Empty;
        Render();
    }

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        _icon.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        _button.Padding = compact ? new Thickness(6, 0, 6, 0) : new Thickness(8, 0, 8, 0);
        RaiseContentWidthChanged();
    }

    private void CaptureTargetWindow()
    {
        var foreground = GetForegroundWindow();
        if (foreground == 0) return;
        _ = GetWindowThreadProcessId(foreground, out var pid);
        if (pid != 0 && pid != (uint)Environment.ProcessId)
        {
            _lastTargetWindow = foreground;
        }
    }

    private void RefreshActiveLayout()
    {
        var active = ReadActiveLayout(_lastTargetWindow);
        _active = active;
        Render();
    }

    private void Render()
    {
        var label = UiText.Get("WidgetKeyboardLayout");
        var key = $"{_active.Handle}|{_active.ShortCode}|{_active.DisplayName}|{label}";
        if (key == _lastRenderKey) return;
        _lastRenderKey = key;

        _codeText.Text = _active.ShortCode;
        _button.ToolTip = $"{label} · {_active.DisplayName}";
        AutomationProperties.SetName(_button, $"{label}: {_active.DisplayName}");
    }

    private void Button_Click(object sender, RoutedEventArgs e)
    {
        CaptureTargetWindow();
        _flyout.Toggle(_button, BuildFlyoutContent);
    }

    private void Button_PreviewMouseWheel(object sender, MouseWheelEventArgs e)
    {
        var layouts = ReadInstalledLayouts();
        if (layouts.Count < 2) return;

        var currentIndex = -1;
        for (var i = 0; i < layouts.Count; i++)
        {
            if (layouts[i].Handle == _active.Handle)
            {
                currentIndex = i;
                break;
            }
        }
        var step = e.Delta < 0 ? 1 : -1;
        var nextIndex = ((currentIndex < 0 ? 0 : currentIndex) + step + layouts.Count) % layouts.Count;
        SelectLayout(layouts[nextIndex]);
        e.Handled = true;
    }

    internal UIElement BuildFlyoutContent()
    {
        var root = new StackPanel { Width = 240 };
        root.Children.Add(DockFlyout.Header(UiText.Get("WidgetKeyboardLayout"), bottomMargin: 6));

        var layouts = ReadInstalledLayouts();
        if (layouts.Count == 0)
        {
            root.Children.Add(DockFlyout.Caption(_active.DisplayName));
            return root;
        }

        foreach (var layout in layouts)
        {
            var selected = layout.Handle == _active.Handle;
            var row = new DockPanel();

            var badge = new Border
            {
                CornerRadius = new CornerRadius(5),
                Padding = new Thickness(6, 2, 6, 2),
                Margin = new Thickness(0, 0, 10, 0),
                VerticalAlignment = VerticalAlignment.Center
            };
            badge.SetResourceReference(Border.BackgroundProperty, selected ? "WidgetSelectedFill" : "SurfaceSelected");
            var code = new TextBlock
            {
                Text = layout.ShortCode,
                FontSize = 11,
                FontWeight = FontWeights.Bold
            };
            code.SetResourceReference(TextBlock.ForegroundProperty, selected ? "WidgetSelectedForeground" : "TextPrimary");
            badge.Child = code;
            DockPanel.SetDock(badge, Dock.Left);
            row.Children.Add(badge);

            var name = new TextBlock
            {
                Text = layout.DisplayName,
                FontSize = 12.5,
                FontWeight = selected ? FontWeights.SemiBold : FontWeights.Normal,
                VerticalAlignment = VerticalAlignment.Center,
                TextTrimming = TextTrimming.CharacterEllipsis
            };
            name.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            row.Children.Add(name);

            var item = DockFlyout.FlatButton(row, (_, _) => SelectLayout(layout), layout.DisplayName);
            item.Padding = new Thickness(8, 7, 8, 7);
            if (selected)
            {
                item.SetResourceReference(Control.BackgroundProperty, "SurfaceSelected");
            }
            root.Children.Add(item);
        }

        return root;
    }

    private void SelectLayout(KeyboardLayoutService.KeyboardLayoutInfo layout)
    {
        _flyout.Close(restoreFocus: true);
        if (ApplyLayout(_lastTargetWindow, layout.Handle))
        {
            _active = layout;
            _lastRenderKey = string.Empty;
            Render();
        }
    }

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint hWnd, out uint lpdwProcessId);
}
