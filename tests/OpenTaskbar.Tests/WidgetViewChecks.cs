using System.IO;
using System.Runtime.ExceptionServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Peers;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;

internal static class WidgetViewChecks
{
    // Mirrors App.xaml's modular resource order so UI tests load exactly what the app loads.
    private static readonly string[] ModularResources =
    [
        "Tokens/Typography", "Tokens/Spacing", "Tokens/Radii", "Tokens/Layout", "Tokens/Motion", "Tokens/Effects",
        "Palettes/Dark",
        "Icons/Navigation", "Icons/Actions", "Icons/Status", "Icons/Media", "Icons/Brand", "Icons/GeometryTemplate",
        "Controls/Tooltip", "Controls/ScrollBar",
        "DockControls", "ContextMenus", "Widgets", "WidgetControls", "SettingsControls"
    ];

    public static int Run()
    {
        Exception? failure = null;
        var checks = 0;
        var thread = new Thread(() =>
        {
            try { checks = RunOnDispatcher(); }
            catch (Exception ex) { failure = ex; }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (failure is not null) ExceptionDispatchInfo.Capture(failure).Throw();
        return checks;
    }

    private static int RunOnDispatcher()
    {
        var checks = 0;
        var app = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
        try
        {
            foreach (var resource in ModularResources)
                app.Resources.MergedDictionaries.Add(new ResourceDictionary
                {
                    Source = new Uri($"/OpenTaskbar;component/Design/{resource}.xaml", UriKind.Relative)
                });
            ThemeManager.Apply(AppTheme.Dark);
            UiText.Current.SetLanguage(AppLanguage.Turkish);
            OpenTaskbar.Tests.WidgetScreenshotCapture.CaptureAll(app);
            checks += DockLayoutChecks.Run(app);
            checks += ApplicationInteractionChecks.Run(app);
            checks += VisualParityChecks.Run(app);
            checks += FeatureAndMemoryChecks.Run(app);
            checks += SettingsAndAutoHideChecks.Run(app);
            checks += AdaptiveWidgetAndConfirmChecks.Run(app);
            checks += NetSpeedChecks.Run(app);
            checks += BluetoothBatteryChecks.Run(app);
            checks += SystemStatsAvailabilityChecks.Run(app);

            var cardStyle = (Style)app.FindResource("WidgetCardButton");
            var cardTemplate = (ControlTemplate)cardStyle.Setters.OfType<Setter>()
                .Single(setter => setter.Property == Control.TemplateProperty).Value;
            Check(!cardTemplate.Triggers.OfType<Trigger>()
                    .Any(trigger => trigger.Property == UIElement.IsKeyboardFocusedProperty),
                "Widget cards must not paint a focus border when clicked.");

            // Hover feedback must fade in/out instead of snapping the surface instantly.
            static bool HasHoverFade(ControlTemplate template) =>
                template.Triggers.OfType<EventTrigger>().Any(t => t.RoutedEvent == UIElement.MouseEnterEvent)
                && template.Triggers.OfType<EventTrigger>().Any(t => t.RoutedEvent == UIElement.MouseLeaveEvent);
            foreach (var key in new[] { "WidgetCardButton", "WidgetIconButton", "WidgetPill", "FlyoutButton", "WidgetButton" })
            {
                var template = (ControlTemplate)((Style)app.FindResource(key)).Setters.OfType<Setter>()
                    .Single(s => s.Property == Control.TemplateProperty).Value;
                Check(HasHoverFade(template), $"{key} must animate hover with MouseEnter/MouseLeave EventTriggers.");
            }
            var borderStyle = (Style)app.FindResource("WidgetCardBorder");
            var borderTriggers = borderStyle.Triggers.OfType<EventTrigger>().ToList();
            Check(borderTriggers.Any(t => t.RoutedEvent == UIElement.MouseEnterEvent)
                  && borderTriggers.Any(t => t.RoutedEvent == UIElement.MouseLeaveEvent),
                "WidgetCardBorder must animate its surface color on hover.");
            var borderA = new Border { Style = borderStyle };
            var borderB = new Border { Style = borderStyle };
            Check(!ReferenceEquals(borderA.Background, borderB.Background),
                "Each widget card must own its background brush so hover animations stay isolated.");

            var interfaceFont = (FontFamily)app.FindResource("InterfaceFont");
            Check(interfaceFont.Source.Contains("#Inter", StringComparison.OrdinalIgnoreCase),
                "The UI must use the embedded Inter font instead of a machine-dependent system font.");
            Check(((FontFamily)app.FindResource("WidgetActionFont")).Source.Contains("MDL2", StringComparison.OrdinalIgnoreCase),
                "Segoe MDL2 Assets must remain the icon font.");

            var shellStyle = (Style)app.FindResource("ShellDockButton");
            var shellTemplate = (ControlTemplate)shellStyle.Setters.OfType<Setter>()
                .Single(setter => setter.Property == Control.TemplateProperty).Value;
            var hoverTrigger = shellTemplate.Triggers.OfType<Trigger>()
                .SingleOrDefault(t => t.Property == UIElement.IsMouseOverProperty);
            Check(hoverTrigger?.EnterActions.Any(action => action is BeginStoryboard) == true
                  && hoverTrigger.ExitActions.Any(action => action is BeginStoryboard),
                "Shell buttons must fade a quiet surface in and out on hover.");

            foreach (var (key, height) in new[]
                     {
                         ("LogoCopilot", 20.04), ("LogoDeepSeek", 17.66), ("LogoOpenRouter", 20.31)
                     })
            {
                var geometry = (System.Windows.Media.StreamGeometry)app.FindResource(key);
                Check(Math.Abs(geometry.Bounds.Width - 24) < 0.5 && Math.Abs(geometry.Bounds.Height - height) < 0.5,
                    $"{key} must keep the official 24x24 brand silhouette.");
            }

            // The widget catalogue is derived from registered widget definitions.
            var availableWidgets = new DockWidgetService(TimeProvider.System).CreateOptions([], []);
            Check(availableWidgets.Any(x => x.Id == "Clock" && x.Category == "Time & Weather")
                && availableWidgets.Any(x => DockWidgetService.AiUsageWidgets.Any(ai => ai.Id == x.Id) && x.Category == "AI Quotas"),
                "Widget categories must be derived from the real catalogue.");
            var expanded = availableWidgets.First(x => x.Id == "Clock");
            var adjacent = availableWidgets.First(x => x.Id == "Weather");
            expanded.IsSettingsExpanded = true;
            var catalogueGrid = new OpenTaskbar.Views.ExpandableWidgetGrid { MinItemWidth = 270 };
            var expandedItem = new Border { DataContext = expanded, Height = 120 };
            var otherItem = new Border { DataContext = adjacent, Height = 45 };
            catalogueGrid.Children.Add(expandedItem);
            catalogueGrid.Children.Add(otherItem);
            catalogueGrid.Measure(new Size(620, double.PositiveInfinity));
            catalogueGrid.Arrange(new Rect(0, 0, 620, 220));
            Check(Math.Abs(expandedItem.ActualWidth - 620) < 1
                && otherItem.TranslatePoint(new Point(), catalogueGrid).Y >= expandedItem.ActualHeight,
                "An expanded widget must span the full row and push following items downward.");

            var settingsAction = new Button { Content = UiText.Get("Apply") };
            var selectedTab = new TabItem { Header = UiText.Get("General"), Content = settingsAction };
            var settingsTabs = new TabControl { Items = { selectedTab }, SelectedIndex = 0 };
            Layout(settingsTabs);
            var tabPeer = UIElementAutomationPeer.CreatePeerForElement(settingsTabs);
            var selectedPeer = tabPeer?.GetChildren()?.Single();
            Check(selectedPeer?.GetChildren()?.Any(peer => peer.GetAutomationControlType() == AutomationControlType.Button) == true,
                "The selected settings page must expose its controls to assistive technology.");

            void Check(bool condition, string name)
            {
                if (!condition) throw new InvalidOperationException(name);
                checks++;
            }

            var clock = new TestClock();
            var timer = new CountdownDockWidget(clock);
            var timerHost = new ContentControl { Content = timer };
            Layout(timerHost);
            var timerTile = RequiredChild<TimedWidgetTile>(timerHost);
            var action = RequiredName<Button>(timerTile, "QuickActionButton");
            var details = RequiredName<Button>(timerTile, "DetailsButton");
            var detailRequests = 0;
            timerHost.AddHandler(TimedWidgetTile.DetailsRequestedEvent,
                new RoutedEventHandler((_, _) => detailRequests++));
            action.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Check(detailRequests == 1 && !timer.IsRunning, "An unconfigured tile must open details instead of starting.");
            Check(AutomationProperties.GetName(action) == UiText.Get("WidgetSetDuration"), "The unconfigured action must have an accessible configure label.");
            Check(RequiredName<TextBlock>(timerTile, "ValueText").Text == UiText.Get("WidgetSetDuration"), "A blank duration must have an explicit visible prompt.");

            timer.MinutesText = "1";
            action.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Layout(timerHost);
            Check(timer.IsRunning && detailRequests == 1, "A configured quick action must start without opening details.");
            Check(AutomationProperties.GetName(action) == UiText.Get("TimedWidgetPause"), "The running action must have an accessible pause label.");
            clock.Advance(TimeSpan.FromSeconds(15));
            timer.Refresh();
            action.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Check(timer.State == CountdownState.Paused && timer.Value == "00:00:45", "The tile must pause the same countdown model.");
            details.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Check(detailRequests == 2, "The value area must open details independently of the quick action.");
            Check(Descendants<TextBox>(timerTile).Count() == 0, "The dock tile must contain no duration form.");
            Check(Descendants<Button>(timerTile).Count() == 2, "The dock tile must offer one detail target and one quick action.");
            Check(action.ActualWidth >= 36 && action.ActualHeight >= 36, "The quick action must keep its pointer target.");
            Check(timerHost.DesiredSize.Width < 200 && timerHost.DesiredSize.Height < 60, "The compact timer must fit within the dock shelf.");

            var panel = new TimedWidgetPanel { DataContext = timer };
            var frame = new WidgetPanelFrame { Heading = UiText.Get("WidgetCountdown"), Body = panel };
            Layout(frame);
            var duration = Descendants<TextBox>(panel).Single();
            Check(duration.IsReadOnly, "Paused duration must remain locked until reset.");
            var panelButtons = Descendants<Button>(panel).ToArray();
            panelButtons.Single(button => Equals(button.Content, UiText.Get("TimedWidgetReset")))
                .RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Layout(frame);
            Check(timer.State == CountdownState.Ready && !duration.IsReadOnly, "Panel reset must unlock the configured duration.");
            duration.Text = "0";
            duration.GetBindingExpression(TextBox.TextProperty)?.UpdateSource();
            Layout(frame);
            Check(timer.HasValidationError && !RequiredName<Button>(panel, "PrimaryActionButton").IsEnabled,
                "Invalid panel input must disable Start through the bound model.");
            Check(Descendants<TextBlock>(panel).Any(text => text.Text == timer.StatusText), "The validation error must be visible in the panel.");
            duration.Text = "1";
            duration.GetBindingExpression(TextBox.TextProperty)?.UpdateSource();
            RequiredName<Button>(panel, "PrimaryActionButton").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Layout(frame);
            Check(timer.IsRunning && RequiredName<TextBlock>(timerTile, "ValueText").Text == "00:01:00",
                "Panel actions and tile display must share one timer state.");

            var stopwatch = new StopwatchDockWidget(clock);
            stopwatch.Refresh();
            var stopwatchHost = new ContentControl { Content = stopwatch };
            Layout(stopwatchHost);
            var stopwatchTile = RequiredChild<TimedWidgetTile>(stopwatchHost);
            RequiredName<Button>(stopwatchTile, "QuickActionButton").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            clock.Advance(TimeSpan.FromHours(26));
            stopwatch.Refresh();
            timer.Refresh();
            Layout(stopwatchHost);
            Layout(timerHost);
            Check(stopwatch.Value == "26:00:00" && timer.State == CountdownState.Finished, "Both compact views must retain independent timer behavior.");
            Check(timerTile.Title == UiText.Get("CountdownFinished"), "The completed tile must display a non-color completion label.");

            UiText.Current.SetLanguage(AppLanguage.English);
            Layout(timerHost);
            Layout(stopwatchHost);
            Check(stopwatchTile.Title == "Stopwatch" && timerTile.Title == "Time is up", "Tile titles must update when language changes.");
            Check(AutomationProperties.GetName(RequiredName<Button>(stopwatchTile, "QuickActionButton")) == "Pause",
                "Quick action names must update when language changes.");
            UiText.Current.SetLanguage(AppLanguage.Turkish);

            var closeRequests = 0;
            frame.CloseRequested += (_, _) => closeRequests++;
            Descendants<Button>(frame).Single(button => AutomationProperties.GetName(button) == UiText.Get("Close"))
                .RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            Check(closeRequests == 1, "The shared panel frame must expose its close request.");
            var stopwatchPanel = new TimedWidgetPanel { DataContext = stopwatch };
            Layout(stopwatchPanel);
            Check(!Descendants<TextBox>(stopwatchPanel).Any(), "The stopwatch panel must not instantiate a countdown form.");
            frame.MaxWidth = 220;
            Layout(frame);
            Check(frame.ActualWidth <= 220 && Descendants<Button>(panel).All(button =>
            {
                var left = button.TranslatePoint(new Point(), frame).X;
                return button.ActualWidth > 0 && left >= 0 && left + button.ActualWidth <= frame.ActualWidth;
            }),
                "A narrow panel must retain reachable action controls.");
            frame.MaxWidth = double.PositiveInfinity;

            foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
            {
                ThemeManager.Apply(theme);
                double Luminance(Color color)
                {
                    double Linear(byte channel) => channel / 255d <= 0.04045
                        ? channel / 255d / 12.92 : Math.Pow((channel / 255d + 0.055) / 1.055, 2.4);
                    return 0.2126 * Linear(color.R) + 0.7152 * Linear(color.G) + 0.0722 * Linear(color.B);
                }
                double Contrast(string foreground, string background)
                {
                    var first = Luminance(((SolidColorBrush)app.Resources[foreground]).Color);
                    var second = Luminance(((SolidColorBrush)app.Resources[background]).Color);
                    return (Math.Max(first, second) + 0.05) / (Math.Min(first, second) + 0.05);
                }
                Check(Contrast("TextPrimary", "SurfacePopup") >= 7, "Panel primary text must retain strong contrast in both themes.");
                Check(Contrast("TextSecondary", "SurfacePopup") >= 4.5, "Panel supporting text must meet normal-text contrast.");
                Check(Contrast("AccentText", "AccentBrush") >= 4.5, "Quick action glyphs must contrast with their accent surface.");
            }

            foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
            foreach (var language in new[] { AppLanguage.Turkish, AppLanguage.English })
            {
                ThemeManager.Apply(theme);
                UiText.Current.SetLanguage(language);
                var options = new DockWidgetService(clock).CreateOptions([], []);
                foreach (var option in options)
                {
                    option.Preview.Value = option.Preview is CountdownDockWidget ? string.Empty : "00:00:00";
                    var card = new WidgetCatalogItem { DataContext = option };
                    Layout(card);
                    var expand = RequiredName<Button>(card, "PreviewButton");
                    var install = RequiredName<Button>(card, "InstallButton");
                    var preview = Descendants<ContentControl>(card)
                        .Single(control => ReferenceEquals(control.Content, option.NativePreview));
                    Check(!preview.IsHitTestVisible
                        && System.Windows.Input.KeyboardNavigation.GetTabNavigation(preview)
                            == System.Windows.Input.KeyboardNavigationMode.None,
                        "Expanded previews must remain non-interactive.");
                    Check(AutomationProperties.GetName(expand) == option.Label,
                        "Expandable widget rows must announce the widget label.");
                    Check(!Descendants<TextBlock>(card).Any(text => text.Text == option.Description),
                        "Compact rows must omit verbose widget descriptions.");
                    var expansions = 0;
                    card.SettingsRequested += (_, _) => expansions++;
                    expand.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(expansions == 1 && !option.IsInstalled,
                        "Expanding a widget must not automatically install it.");
                    var installations = 0;
                    card.InstallationRequested += (_, _) => installations++;
                    install.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(installations == 1 && !option.IsInstalled,
                        "The dedicated management action must request installation.");
                    option.UpdateText("Localized widget", "Localized description");
                    Check(option.Preview.Label == option.Label, "Preview labels must follow catalog localization.");
                }

                foreach (var width in new[] { 292d, 500d, 720d })
                {
                    var gallery = new OpenTaskbar.Views.ExpandableWidgetGrid { Width = width, MinItemWidth = 270 };
                    foreach (var option in options) gallery.Children.Add(new WidgetCatalogItem { DataContext = option });
                    Layout(gallery);
                    Check(gallery.Children.Cast<FrameworkElement>().All(item =>
                    {
                        var point = item.TranslatePoint(new Point(), gallery);
                        return point.X >= 0 && point.X + item.ActualWidth <= width;
                    }), "The widget catalogue must avoid horizontal overflow.");
                    var second = (FrameworkElement)gallery.Children[1];
                    Check(second.TranslatePoint(new Point(), gallery).Y > 0,
                        "The widget catalogue must use a single full-width column for every category.");
                }
            }
            UiText.Current.SetLanguage(AppLanguage.Turkish);

            timer.Reset();
            timer.Toggle();
            clock.Advance(TimeSpan.FromSeconds(15));
            timer.Refresh();
            stopwatch.Reset();
            stopwatch.Toggle();
            clock.Advance(TimeSpan.FromSeconds(24));
            stopwatch.Refresh();
            timer.Refresh();
            var directory = Path.Combine(Path.GetTempPath(), "OpenTaskbarWidgetComponents");
            Directory.CreateDirectory(directory);
            foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
            {
                ThemeManager.Apply(theme);
                var themeEditor = new OpenTaskbar.Views.ThemePropertyEditor();
                themeEditor.LoadTheme(null, theme);
                Layout(themeEditor);
                Check(Descendants<TextBlock>(themeEditor).Any(text => text.Text == UiText.Get("ColorPalette"))
                    && Descendants<Button>(themeEditor).Any(button => button.Content is string label
                        && label == UiText.Get("ThemeCreateEditable")),
                    "Default appearance must show color swatches and customization controls.");
                var solidPicker = new OpenTaskbar.Views.ThemeBrushPicker(
                    new SolidColorBrush(Color.FromArgb(128, 220, 80, 40)));
                var solidPickerContent = (FrameworkElement)solidPicker.Content;
                solidPicker.Content = null;
                var solidSurface = new Border { Background = (Brush)app.Resources["SettingsBackground"], Child = solidPickerContent };
                Layout(solidSurface);
                var solidOpacity = Descendants<Slider>(solidPickerContent).Single(slider =>
                    AutomationProperties.GetName(slider) == "Color opacity");
                Check(solidOpacity.Value == 128, "Solid color opacity must load into the picker.");
                solidOpacity.Value = 64;
                Check(Descendants<TextBox>(solidPickerContent).Single().Text == "#40DC5028",
                    "Changing solid color opacity must preserve RGB and update the alpha channel.");
                Render(solidSurface, Path.Combine(directory, $"color-picker-solid-{theme.ToString().ToLowerInvariant()}.png"));

                var gradient = new LinearGradientBrush();
                gradient.GradientStops.Add(new GradientStop(Color.FromArgb(96, 25, 80, 140), 0));
                gradient.GradientStops.Add(new GradientStop(Color.FromArgb(200, 180, 90, 40), 1));
                var gradientPicker = new OpenTaskbar.Views.ThemeBrushPicker(gradient);
                var gradientPickerContent = (FrameworkElement)gradientPicker.Content;
                gradientPicker.Content = null;
                var gradientSurface = new Border { Background = (Brush)app.Resources["SettingsBackground"], Child = gradientPickerContent };
                Layout(gradientSurface);
                var gradientOpacity = Descendants<Slider>(gradientPickerContent).Single(slider =>
                    AutomationProperties.GetName(slider) == "Color opacity");
                Check(gradientOpacity.Value == 96, "Gradient stop opacity must load into the picker.");
                Check(Descendants<Button>(gradientPickerContent).Any(button => button.Content is string label
                    && label.StartsWith("Stop 1 · #6019508C · 0%", StringComparison.Ordinal)
                    && button.Visibility == Visibility.Visible && button.ActualHeight > 0),
                    "Gradient stops must display their color and editable position.");
                gradientOpacity.Value = 48;
                Check(Descendants<TextBox>(gradientPickerContent).Single().Text == "#3019508C",
                    "Changing gradient stop opacity must preserve the stop's RGB values.");
                Render(gradientSurface, Path.Combine(directory, $"color-picker-gradient-{theme.ToString().ToLowerInvariant()}.png"));
                var strip = new StackPanel { Orientation = Orientation.Horizontal };
                strip.Children.Add(new ContentControl { Content = stopwatch });
                strip.Children.Add(new ContentControl { Content = timer });
                var preview = new StackPanel { Margin = new Thickness(16) };
                preview.Children.Add(strip);
                preview.Children.Add(new WidgetPanelFrame
                {
                    Heading = UiText.Get("WidgetCountdown"), Body = new TimedWidgetPanel { DataContext = timer },
                    HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 12, 0, 0)
                });
                var surface = new Border { Background = (Brush)app.Resources["SettingsBackground"], Child = preview };
                Layout(surface);
                Render(surface, Path.Combine(directory, $"timed-widgets-{theme.ToString().ToLowerInvariant()}.png"));
                var catalog = new WrapPanel { Width = 500, Margin = new Thickness(20) };
                var previewOptions = new DockWidgetService(clock).CreateOptions([DockWidgetService.StopwatchId], [DockWidgetService.StopwatchId]);
                SetPreviewFixtures(previewOptions);
                foreach (var option in previewOptions)
                {
                    catalog.Children.Add(new WidgetCatalogItem { DataContext = option });
                }
                var catalogSurface = new Border { Background = (Brush)app.Resources["SettingsBackground"], Child = catalog };
                Layout(catalogSurface);
                Render(catalogSurface, Path.Combine(directory, $"widget-catalog-{theme.ToString().ToLowerInvariant()}.png"));
                var settingsPath = Path.Combine(Path.GetTempPath(), $"OpenTaskbarGallery-{Guid.NewGuid():N}.json");
                var preferences = new ApplicationPreferences(new DockSettingsStore(settingsPath));
                Check(preferences.Save(preferences.Current with { Theme = theme, Language = AppLanguage.Turkish }) is null,
                    "The isolated settings fixture must save successfully.");
                var settings = new OpenTaskbar.SettingsWindow(preferences);
                try
                {
                    // Segment choices are ComboBoxes whose selected item Tag carries the enum value. Verify the
                    // real window maps its loaded settings onto the selected items instead of leaving them blank.
                    string SelectedTag(string name) =>
                        (RequiredName<ComboBox>(settings, name).SelectedItem as ComboBoxItem)?.Tag as string ?? string.Empty;
                    Check(SelectedTag("ThemeBox") == theme.ToString(), "Theme dropdown must reflect the loaded theme.");
                    Check(SelectedTag("EdgeBox") == "Bottom", "Dock edge dropdown must reflect the default edge.");
                    Check(SelectedTag("SizeBox") == "Compact", "Dock size dropdown must reflect the default size.");
                    Check(SelectedTag("AutoHideBox") == "Disabled", "Auto-hide dropdown must reflect the default mode.");
                    Check(SelectedTag("PositionBox") == "Center", "Dock alignment dropdown must reflect the default position.");
                    Check(SelectedTag("DockSurfaceBox") == "Glass", "Surface dropdown must reflect the loaded material.");
                    RequiredName<TabControl>(settings, "SettingsTabs").SelectedItem = RequiredName<TabItem>(settings, "WidgetsTab");
                    var list = RequiredName<ItemsControl>(settings, "WidgetOptionsList");
                    var selections = ((System.Windows.Data.ListCollectionView)list.ItemsSource).SourceCollection.Cast<DockWidgetOption>().ToArray();
                    SetPreviewFixtures(selections);
                    var page = (FrameworkElement)settings.Content;
                    foreach (var width in new[] { 560d, 740d })
                    {
                        page.Width = width;
                        page.Height = 598;
                        Layout(page);
                        var cards = Descendants<WidgetCatalogItem>(list).ToArray();
                        Check(cards.Length == selections.Length && cards.All(card =>
                        {
                            var left = card.TranslatePoint(new Point(), list).X;
                            return left >= 0 && left + card.ActualWidth <= list.ActualWidth;
                        }), "The real settings page must keep all previews inside its viewport at default and minimum width.");
                        Render(page, Path.Combine(directory, $"widget-settings-{theme.ToString().ToLowerInvariant()}-{width:0}.png"));
                    }
                    var firstCard = Descendants<WidgetCatalogItem>(list).First();
                    var selection = (DockWidgetOption)firstCard.DataContext;
                    RequiredName<Button>(firstCard, "InstallButton").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(selection.IsInstalled && selection.IsEnabled, "The install button must install and enable the widget.");
                    Check(preferences.Current.InstalledWidgets?.Contains(selection.Id) == true
                        && preferences.Current.Widgets?.Contains(selection.Id) == true,
                        "Installation must auto-save the enabled widget selection.");
                    Layout(page);
                    var currentCard = Descendants<WidgetCatalogItem>(list).Single(card => ReferenceEquals(card.DataContext, selection));
                    RequiredName<Button>(currentCard, "InstallButton").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(!selection.IsInstalled && !selection.IsEnabled, "The install button must remove an installed widget.");
                    Check(preferences.Current.InstalledWidgets?.Contains(selection.Id) != true
                        && preferences.Current.Widgets?.Contains(selection.Id) != true,
                        "Removal must auto-save the widget selection.");

                    var clockCard = Descendants<WidgetCatalogItem>(list).Single(card => ((DockWidgetOption)card.DataContext).Id == DockWidgetService.ClockId);
                    var clockOption = (DockWidgetOption)clockCard.DataContext;
                    var clockExpandBtn = RequiredName<Button>(clockCard, "PreviewButton");
                    clockExpandBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(clockOption.IsSettingsExpanded, "Clicking the widget row must expand it inline.");
                    Check(RequiredName<FrameworkElement>(settings, "ClockOptions").Visibility == Visibility.Visible,
                        "An expanded clock row must show its native settings.");

                    var secondsToggle = RequiredName<CheckBox>(settings, "ClockSecondsToggle");
                    secondsToggle.IsChecked = true;
                    Check(preferences.Current.ClockShowSeconds,
                        "Clock settings must auto-save changes without an apply button.");

                    clockExpandBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(!clockOption.IsSettingsExpanded, "Clicking the row again must collapse it.");
                    Check(RequiredName<FrameworkElement>(settings, "ClockOptions").Visibility == Visibility.Collapsed,
                        "Collapsing the row must hide its settings.");

                    var trayCard = Descendants<WidgetCatalogItem>(list).Single(card => ((DockWidgetOption)card.DataContext).Id == DockWidgetService.NetSpeedId);
                    var trayOption = (DockWidgetOption)trayCard.DataContext;
                    RequiredName<Button>(trayCard, "PreviewButton").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(trayOption.IsSettingsExpanded, "Widgets without custom settings must still offer inline previews.");

                    var weatherCard = Descendants<WidgetCatalogItem>(list).Single(card => ((DockWidgetOption)card.DataContext).Id == DockWidgetService.WeatherId);
                    var weatherOption = (DockWidgetOption)weatherCard.DataContext;
                    var weatherExpandBtn = RequiredName<Button>(weatherCard, "PreviewButton");
                    weatherExpandBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(weatherOption.IsSettingsExpanded, "Weather must expand from its widget row.");
                    Check(weatherOption.SettingsView is not null, "Weather must retain its native settings view.");
                    var countryBox = RequiredName<ComboBox>(settings, "WeatherCountryBox");
                    var cityContainer = RequiredName<FrameworkElement>(settings, "WeatherCityContainer");
                    var cityCombo = RequiredName<ComboBox>(settings, "WeatherCityCombo");
                    Check(countryBox.Visibility == Visibility.Visible, "Weather country box must be visible.");
                    countryBox.SelectedItem = "Turkey";
                    Layout(settings);
                    Check(cityContainer.Visibility == Visibility.Visible, "Selecting a country must make the city dropdown container visible.");
                    Check(cityCombo.Items.Count > 0, "Selecting Turkey must populate Turkish cities.");
                    cityCombo.SelectedItem = "İzmir";
                    Check(preferences.Current.WeatherCity == "İzmir", "Selecting a city in the dropdown must auto-save WeatherCity.");
                    weatherExpandBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                    Check(!weatherOption.IsSettingsExpanded, "Clicking weather settings button again must collapse it.");

                    // Categories are section labels with their widgets underneath, not tabs/filters.
                    Layout(settings);
                    Check(selections.Length == 21, "All 21 widgets must be listed.");
                    Check(selections.Select(option => option.CategoryEnum).Distinct().OrderBy(c => c).SequenceEqual(
                            Enum.GetValues<DockWidgetCategory>().OrderBy(c => c)),
                        "All five widget categories must be represented.");
                    Check(selections.Count(option => option.CategoryEnum == DockWidgetCategory.AiQuotas) == 7,
                        "The AI category must contain all 7 AI widgets.");
                    Check(selections.Count(option => option.CategoryEnum == DockWidgetCategory.SystemAndHardware) == 5,
                        "The system category must contain all 5 system widgets.");
                    var allCards = Descendants<WidgetCatalogItem>(list).ToArray();
                    Check(allCards.Length == 21, "All 21 widgets must be visible at once without any category filter.");
                    Check(settings.FindName("WidgetCategoryAiRadio") is null,
                        "Widget categories must be labels, not a segmented tab filter.");
                }
                finally
                {
                    settings.Close();
                    if (File.Exists(settingsPath)) File.Delete(settingsPath);
                }
            }
            return checks;
        }
        finally
        {
            app.Shutdown();
        }
    }

    private static void SetPreviewFixtures(IEnumerable<DockWidgetOption> options)
    {
        foreach (var option in options)
            option.Preview.Value = option.Id switch
            {
                DockWidgetService.ClockId => "14:32",
                DockWidgetService.DateId => "Pzt, 16 Kas",
                DockWidgetService.MediaId => "▶ · 02:15",
                DockWidgetService.AudioId => "100%",
                DockWidgetService.QuickSettingsId => "Wi-Fi · BT",
                DockWidgetService.SystemStatsId => "CPU · RAM",
                DockWidgetService.FocusId => "25:00",
                DockWidgetService.WeatherId => "21°",
                DockWidgetService.NetSpeedId => "↓ 12 MB/s · ↑ 2 MB/s",
                DockWidgetService.BluetoothBatteryId => "53%",
                var id when id.StartsWith("AiUsage", StringComparison.Ordinal) => string.Empty,
                DockWidgetService.BatteryId => UiText.Get("SystemDataUnavailable"),
                "Storage" => "128,5 GiB",
                "DayProgress" => "61%",
                DockWidgetService.StopwatchId => "00:00:00",
                DockWidgetService.CountdownId => string.Empty,
                _ => throw new InvalidOperationException("Add a render fixture for the new widget.")
            };
    }

    private static void Layout(FrameworkElement element)
    {
        element.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        element.Arrange(new Rect(new Point(), element.DesiredSize));
        element.UpdateLayout();
        element.Dispatcher.Invoke(() => { }, DispatcherPriority.DataBind);
    }

    private static T RequiredName<T>(FrameworkElement owner, string name) where T : FrameworkElement =>
        owner.FindName(name) is T element ? element : throw new InvalidOperationException($"Missing control {name}.");
    private static T RequiredChild<T>(DependencyObject owner) where T : DependencyObject =>
        Descendants<T>(owner).FirstOrDefault() ?? throw new InvalidOperationException($"Missing {typeof(T).Name}.");
    private static IEnumerable<T> Descendants<T>(DependencyObject owner) where T : DependencyObject
    {
        for (var index = 0; index < VisualTreeHelper.GetChildrenCount(owner); index++)
        {
            var child = VisualTreeHelper.GetChild(owner, index);
            if (child is T match) yield return match;
            foreach (var descendant in Descendants<T>(child)) yield return descendant;
        }
    }

    private static void Render(FrameworkElement element, string path)
    {
        var bitmap = new RenderTargetBitmap((int)Math.Ceiling(element.ActualWidth), (int)Math.Ceiling(element.ActualHeight),
            96, 96, PixelFormats.Pbgra32);
        bitmap.Render(element);
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bitmap));
        using var output = File.Create(path);
        encoder.Save(output);
    }
}
