using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>Small modal prompt for a single line of text, styled like the dock menus.</summary>
public static class TextPrompt
{
    public static string? Ask(string title, string message, string initial = "")
    {
        var window = new Window
        {
            Title = title,
            Width = 360,
            SizeToContent = SizeToContent.Height,
            WindowStyle = WindowStyle.None,
            AllowsTransparency = true,
            Background = System.Windows.Media.Brushes.Transparent,
            ResizeMode = ResizeMode.NoResize,
            WindowStartupLocation = WindowStartupLocation.CenterScreen,
            ShowInTaskbar = false,
            Topmost = true
        };
        window.SetResourceReference(Window.FontFamilyProperty, "InterfaceFont");
        var input = new TextBox { Text = initial, Margin = new Thickness(0, 10, 0, 14), Padding = new Thickness(8, 6, 8, 6), FontSize = 13 };
        input.SetResourceReference(Control.BackgroundProperty, "SettingsControl");
        input.SetResourceReference(Control.ForegroundProperty, "TextPrimary");
        input.SetResourceReference(Control.BorderBrushProperty, "MenuStroke");
        input.SetResourceReference(TextBox.CaretBrushProperty, "TextPrimary");
        string? result = null;
        var ok = new Button { Content = UiText.Get("Ok"), IsDefault = true, MinWidth = 88, Margin = new Thickness(8, 0, 0, 0) };
        var cancel = new Button { Content = UiText.Get("Cancel"), IsCancel = true, MinWidth = 88 };
        ok.SetResourceReference(FrameworkElement.StyleProperty, "FlyoutButton");
        cancel.SetResourceReference(FrameworkElement.StyleProperty, "FlyoutButton");
        ok.HorizontalContentAlignment = cancel.HorizontalContentAlignment = HorizontalAlignment.Center;
        ok.Click += (_, _) =>
        {
            result = input.Text;
            window.DialogResult = true;
        };
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
        buttons.Children.Add(cancel);
        buttons.Children.Add(ok);
        var heading = new TextBlock { Text = message, TextWrapping = TextWrapping.Wrap, FontWeight = FontWeights.SemiBold };
        var body = new StackPanel();
        body.Children.Add(heading);
        body.Children.Add(input);
        body.Children.Add(buttons);
        var frame = new Border { CornerRadius = new CornerRadius(12), BorderThickness = new Thickness(1), Padding = new Thickness(18), Child = body };
        frame.SetResourceReference(Border.BackgroundProperty, "MenuMaterial");
        frame.SetResourceReference(Border.BorderBrushProperty, "MenuStrokeMaterial");
        frame.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        frame.MouseLeftButtonDown += (_, args) =>
        {
            if (args.ButtonState == MouseButtonState.Pressed) window.DragMove();
        };
        window.Content = frame;
        window.SourceInitialized += (_, _) =>
        {
            var helper = new System.Windows.Interop.WindowInteropHelper(window);
            MainWindow.ExcludeFromTaskSwitcher(helper.Handle);
        };
        window.Loaded += (_, _) =>
        {
            input.Focus();
            input.SelectAll();
        };
        return window.ShowDialog() == true ? result : null;
    }
}
