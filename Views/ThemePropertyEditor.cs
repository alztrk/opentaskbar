using System;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views;

/// <summary>
/// Flat, token-based editor that writes the real theme.json/widgets/pack.json format.
/// Built-in theme packs are read-only; duplicate them before editing.
/// </summary>
public sealed class ThemePropertyEditor : UserControl
{
    private static readonly (string Title, string[] Keys)[] Sections =
    [
        ("Colors", ["TextPrimary", "TextSecondary", "AccentBrush", "AccentText", "StrokeSubtle",
                    "StrokeFocus", "SurfaceSelected", "SurfaceHover", "SurfacePressed"]),
        ("Dock", ["DockBackgroundMode", "DockMaterial", "DockBorderMaterial", "DockHighlight",
                  "DockRadius", "DockInnerRadius", "DockWidgetPadding", "DockShadowBlur", "DockShadowOpacity", "DockShadowColor"]),
        ("Widgets", ["WidgetCard", "WidgetCardHover", "WidgetCardPressed", "WidgetCardStroke", "WidgetTrack", "WidgetAccent",
                     "DockWidgetSurface", "DockWidgetRadius"]),
        ("Windows & Popups", ["MenuMaterial", "DynamicIslandMaterial", "MenuDivider", "SurfacePopup", "MenuLabelWidth", "PopupRadius", "PopupInnerRadius",
                               "SettingsBackground", "SettingsBackgroundOpacity", "SettingsControl", "SettingsControlHover", "SettingsSidebarOverlay"]),
        ("Icons", ["IconForeground", "WindowsLogoBrush", "IconHoverGlow", "StartTileBackground",
                   "StartTileStroke", "StartTileHover", "WidgetWarning", "WidgetFocusAccent"]),
        ("Typography & Geometry", ["InterfaceFont", "TextSizeBody", "TextSizeCaption",
                                    "ControlRadius", "SettingsPageInset"])
    ];

    private static string SectionTitle(string title) => title switch
    {
        "Colors" => UiText.Get("ThemeSectionColors"),
        "Dock" => UiText.Get("ThemeSectionDock"),
        "Widgets" => UiText.Get("ThemeSectionWidgets"),
        "Windows & Popups" => UiText.Get("ThemeSectionWindows"),
        "Icons" => UiText.Get("ThemeSectionIcons"),
        "Typography & Geometry" => UiText.Get("ThemeSectionTypography"),
        _ => title
    };

    private readonly StackPanel _body = new();
    private Panel _fieldHost;
    private readonly TextBlock _message = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 10, 0, 14) };
    private readonly System.Windows.Threading.DispatcherTimer _autoSaveTimer;
    private bool _loadingTheme;
    private string? _id;
    private AppTheme _mode;
    private JsonObject? _widgetPack;
    private JsonObject? _manifest;
    private TextBox? _name;
    private TextBox? _author;
    private TextBox? _description;

    public event EventHandler? ThemeSaved;
    public event EventHandler<string>? ThemeCreated;

    public ThemePropertyEditor()
    {
        _fieldHost = _body;
        Content = _body;
        Margin = new Thickness(0, 8, 0, 0);
        _message.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        _autoSaveTimer = new System.Windows.Threading.DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(350)
        };
        _autoSaveTimer.Tick += (_, _) =>
        {
            _autoSaveTimer.Stop();
            SaveTheme();
        };
    }

    /// <summary>Commits any pending debounced theme edits immediately (e.g. when Settings closes).</summary>
    public void FlushAutoSave()
    {
        if (_autoSaveTimer.IsEnabled)
        {
            _autoSaveTimer.Stop();
            SaveTheme();
        }
    }

    private void ScheduleAutoSave(bool immediate)
    {
        if (_loadingTheme || _id is null || _widgetPack is null || _manifest is null) return;
        if (immediate)
        {
            _autoSaveTimer.Stop();
            SaveTheme();
        }
        else
        {
            _autoSaveTimer.Stop();
            _autoSaveTimer.Start();
        }
    }

    public void LoadTheme(string? id, AppTheme mode)
    {
        _autoSaveTimer.Stop();
        _loadingTheme = true;
        try
        {
            _id = id;
            _mode = mode == AppTheme.System ? ThemeManager.EffectiveTheme : mode;
            _widgetPack = null;
            _manifest = null;
            _body.Children.Clear();
            _fieldHost = _body;
            _body.Children.Add(_message);

            var pack = ThemePacks.Discover().FirstOrDefault(p => string.Equals(p.Id, id, StringComparison.OrdinalIgnoreCase));
            if (pack is null || ThemePack.IsBuiltInTheme(pack.Id))
            {
                ShowCurrentAppearance(pack?.DisplayName ?? UiText.Get("ThemeWindowsAppearance"));
                return;
            }

            try
            {
                var manifestPath = Path.Combine(pack.Directory, ThemePack.ManifestFileName);
                _manifest = JsonNode.Parse(File.ReadAllText(manifestPath)) as JsonObject ?? new JsonObject();
                var widgetPath = Path.Combine(pack.Directory, ThemePack.WidgetFolder, WidgetPack.ManifestFileName);
                _widgetPack = File.Exists(widgetPath)
                    ? JsonNode.Parse(File.ReadAllText(widgetPath)) as JsonObject ?? new JsonObject()
                    : new JsonObject { ["name"] = pack.DisplayName + " " + UiText.Get("ThemeWidgetPackSuffix") };
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
            {
                _message.Text = UiText.Format("ThemeLoadFailed", ex.Message);
                return;
            }

            _message.Text = UiText.Format("ThemeEditing", pack.DisplayName);
            ShowColorSwatches();
            SectionHeader(UiText.Get("ThemeInformation"));
            _name = AddTextField(UiText.Get("ThemeFieldName"), ReadManifest("Name") ?? pack.DisplayName);
            _author = AddTextField(UiText.Get("ThemeFieldAuthor"), ReadManifest("Author") ?? string.Empty);
            _description = AddTextField(UiText.Get("ThemeFieldDescription"), ReadManifest("Description") ?? string.Empty);
            foreach (var metaField in new[] { _name, _author, _description })
            {
                metaField.TextChanged += (_, _) => ScheduleAutoSave(immediate: false);
                metaField.LostFocus += (_, _) => FlushAutoSave();
            }
            var categoryTabs = new TabControl { MinHeight = 260, Margin = new Thickness(0, 14, 0, 0) };
            categoryTabs.SetResourceReference(FrameworkElement.StyleProperty, "ThemeEditorTabControl");
            _body.Children.Add(categoryTabs);
            foreach (var (title, keys) in Sections)
            {
                var panel = new StackPanel { Margin = new Thickness(0, 2, 0, 8) };
                var tabItem = new TabItem { Header = SectionTitle(title), Content = panel };
                tabItem.SetResourceReference(FrameworkElement.StyleProperty, "ThemeEditorTabItem");
                categoryTabs.Items.Add(tabItem);
                _fieldHost = panel;
                foreach (var key in keys)
                {
                    var existing = ResolveTokenValue(key);
                    if (existing is null) continue;
                    if (key == "DockBackgroundMode") AddModeField(key, existing.ToString() ?? "Glass");
                    else if (existing is Brush brush) AddBrushField(key, brush);
                    else if (existing is CornerRadius radius) AddCornerField(key, radius);
                    else if (existing is Thickness thickness) AddThicknessField(key, thickness);
                    else if (existing is double value) AddNumberField(key, value);
                    else if (existing is FontFamily family) AddFontField(key, family);
                }
            }

            var covered = Sections.SelectMany(section => section.Keys).ToHashSet(StringComparer.Ordinal);
            var advancedFields = new StackPanel { Margin = new Thickness(0, 2, 0, 8) };
            var advancedTab = new TabItem { Header = UiText.Get("ThemeSectionAdvanced"), Content = advancedFields };
            advancedTab.SetResourceReference(FrameworkElement.StyleProperty, "ThemeEditorTabItem");
            categoryTabs.Items.Add(advancedTab);
            _fieldHost = advancedFields;
            foreach (var key in AllThemeKeys(Application.Current.Resources)
                         .Where(key => !covered.Contains(key) && IsEditableAdvancedToken(key))
                         .Distinct(StringComparer.Ordinal)
                         .OrderBy(key => key, StringComparer.OrdinalIgnoreCase))
            {
                var existing = ResolveTokenValue(key);
                if (existing is Brush brush) AddBrushField(key, brush);
                else if (existing is Color color) AddBrushField(key, new SolidColorBrush(color));
                else if (existing is CornerRadius radius) AddCornerField(key, radius);
                else if (existing is Thickness thickness) AddThicknessField(key, thickness);
                else if (existing is double number) AddNumberField(key, number);
                else if (existing is FontFamily family) AddFontField(key, family);
            }
            _fieldHost = _body;
        }
        finally
        {
            _loadingTheme = false;
        }
    }

    private static readonly (string LabelKey, string Key)[] ColorOverview =
    [
        ("ColorSwatchAccent", "AccentBrush"),
        ("ColorSwatchBackground", "SettingsBackground"),
        ("ColorSwatchSurface", "SettingsControl"),
        ("ColorSwatchText", "TextPrimary"),
        ("ColorSwatchSecondary", "TextSecondary"),
        ("ColorSwatchBorder", "StrokeSubtle")
    ];

    private void ShowColorSwatches()
    {
        SectionHeader(UiText.Get("ColorPalette"));
        var strip = new WrapPanel { Margin = new Thickness(0, 0, 0, 3) };
        foreach (var (labelKey, key) in ColorOverview)
        {
            if (ResolveTokenValue(key) is not Brush brush) continue;
            var cell = new StackPanel { Width = 92, Margin = new Thickness(0, 0, 10, 6) };
            cell.Children.Add(new Border
            {
                Background = brush.CloneCurrentValue(),
                Height = 34,
                CornerRadius = new CornerRadius(6),
                BorderThickness = new Thickness(1),
                BorderBrush = Application.Current.TryFindResource("StrokeSubtle") as Brush
            });
            var caption = new TextBlock { Text = UiText.Get(labelKey), FontSize = 11, Margin = new Thickness(0, 7, 0, 0) };
            caption.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            cell.Children.Add(caption);
            strip.Children.Add(cell);
        }
        _body.Children.Add(strip);
    }

    private void ShowCurrentAppearance(string title)
    {
        _message.Text = title;
        ShowColorSwatches();

        // A built-in theme is read-only. One clear action is enough — fake
        // "surface preview" boxes only added clutter without any function.
        SectionHeader(UiText.Get("CustomizeAppearance"));
        var create = Button(UiText.Get("ThemeCreateEditable"));
        create.Style = (Style)Application.Current.FindResource("SettingsPrimaryButton");
        create.HorizontalAlignment = HorizontalAlignment.Left;
        create.Click += (_, _) =>
        {
            try
            {
                var id = ThemePackTransfer.CreateTheme(UiText.Format("ThemeNewEditableName",
                    UiText.Get(_mode == AppTheme.Light ? "Light" : "Dark")),
                    null, _id);
                ThemeCreated?.Invoke(this, id);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                _message.Text = UiText.Format("ThemeCreationFailed", ex.Message);
            }
        };
        _body.Children.Add(create);
    }

    private static readonly HashSet<string> EditableAdvancedTokens = new(StringComparer.Ordinal)
    {
        "AccentTrack", "CaptionCloseForeground", "DangerText", "DockInnerBevelMaterial",
        "DockScrollScrimStart", "DockScrollScrimEnd", "DockScrollScrimTop", "DockScrollScrimBottom",
        "DockWidgetActionRadius", "IndicatorBrush", "IndicatorForegroundBrush", "MenuStroke",
        "NotificationBrush", "SettingsTrack", "ShellButtonHover", "ShellButtonPressed",
        "StartTilePressed", "SurfaceDanger", "SurfaceKeyboardFocus", "SurfacePressed",
        "SurfaceSelectedStrong", "TextMuted", "WidgetAccentFill", "WidgetAccentText",
        "WidgetBlue", "WidgetCardHover", "WidgetCardPressed", "WidgetDangerForeground", "WidgetDangerHover",
        "WidgetDangerStroke", "WidgetFieldRadius", "WidgetGlassCard", "WidgetGlassCardHover",
        "WidgetGlassCardStroke", "WidgetGreen", "WidgetIsland", "WidgetIslandStroke",
        "WidgetOnStroke", "WidgetQuickActionRadius", "WidgetRadius", "WidgetSelectedFill",
        "WidgetSelectedForeground", "WidgetSelectedStroke"
    };

    private static bool IsEditableAdvancedToken(string key) => EditableAdvancedTokens.Contains(key);

    private static IEnumerable<string> AllThemeKeys(ResourceDictionary dictionary)
    {
        foreach (var key in dictionary.Keys)
            if (key is string text) yield return text;
        foreach (var child in dictionary.MergedDictionaries)
            foreach (var key in AllThemeKeys(child)) yield return key;
    }

    private void SectionHeader(string title)
    {
        var stack = new StackPanel { Margin = new Thickness(0, 20, 0, 8) };
        var heading = new TextBlock
        {
            Text = title,
            FontWeight = FontWeights.SemiBold,
            FontSize = 12,
            Margin = new Thickness(0, 0, 0, 8)
        };
        heading.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        stack.Children.Add(heading);
        var divider = new Border { Height = 1 };
        divider.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
        stack.Children.Add(divider);
        _fieldHost.Children.Add(stack);
    }

    private Grid AddRow(string label)
    {
        var rowBorder = new Border
        {
            BorderThickness = new Thickness(0, 0, 0, 1),
            Padding = new Thickness(0, 8, 0, 8),
            MinHeight = 46,
            SnapsToDevicePixels = true
        };
        rowBorder.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(210) });
        var text = new TextBlock
        {
            Text = PrettyName(label),
            FontSize = 12.5,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 12, 0),
            TextWrapping = TextWrapping.Wrap
        };
        text.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        grid.Children.Add(text);
        rowBorder.Child = grid;
        _fieldHost.Children.Add(rowBorder);
        return grid;
    }

    private static readonly Dictionary<string, string> TokenLabels = new(StringComparer.OrdinalIgnoreCase)
    {
        ["TextPrimary"] = "Primary text",
        ["TextSecondary"] = "Secondary text",
        ["AccentBrush"] = "Accent color",
        ["AccentText"] = "Accent text color",
        ["StrokeSubtle"] = "Border color",
        ["StrokeFocus"] = "Focus ring color",
        ["SurfaceSelected"] = "Selected surface",
        ["SurfaceHover"] = "Hover surface",
        ["SurfacePressed"] = "Pressed surface",
        ["DockBackgroundMode"] = "Dock material",
        ["DockMaterial"] = "Dock background",
        ["DockBorderMaterial"] = "Dock border",
        ["DockHighlight"] = "Dock highlight",
        ["DockRadius"] = "Dock corner radius",
        ["DockInnerRadius"] = "Dock inner radius",
        ["DockWidgetPadding"] = "Widget padding",
        ["DockShadowBlur"] = "Shadow blur",
        ["DockShadowOpacity"] = "Shadow opacity",
        ["DockShadowColor"] = "Shadow color",
        ["WidgetCard"] = "Widget surface",
        ["WidgetCardHover"] = "Widget card hover",
        ["WidgetCardPressed"] = "Widget card pressed",
        ["WidgetCardStroke"] = "Widget border",
        ["WidgetTrack"] = "Widget meter track",
        ["WidgetAccent"] = "Widget meter fill",
        ["IconForeground"] = "Utility icon color",
        ["WindowsLogoBrush"] = "Start logo color",
        ["IconHoverGlow"] = "Icon hover glow",
        ["StartTileBackground"] = "Start tile background",
        ["StartTileStroke"] = "Start tile border",
        ["StartTileHover"] = "Start tile hover",
        ["WidgetWarning"] = "Widget warning color",
        ["WidgetFocusAccent"] = "Widget focus color",
        ["DockWidgetSurface"] = "Dock widget background",
        ["DockWidgetRadius"] = "Dock widget radius",
        ["WidgetSpacing"] = "Widget spacing",
        ["WidgetBackground"] = "Widget background",
        ["SettingsBackground"] = "Settings window background",
        ["SettingsBackgroundOpacity"] = "Settings window opacity",
        ["SettingsControl"] = "Control surface",
        ["SettingsControlHover"] = "Control hover surface",
        ["SettingsSidebarOverlay"] = "Sidebar surface",
        ["MenuMaterial"] = "Menu background",
        ["DynamicIslandMaterial"] = "Dynamic Island background",
        ["MenuDivider"] = "Menu divider",
        ["SurfacePopup"] = "Popup surface",
        ["MenuLabelWidth"] = "Menu label width",
        ["PopupRadius"] = "Popup corner radius",
        ["PopupInnerRadius"] = "Popup inner radius",
        ["InterfaceFont"] = "Font family",
        ["TextSizeBody"] = "Body font size",
        ["TextSizeCaption"] = "Caption font size",
        ["ControlRadius"] = "Control corner radius",
        ["SettingsPageInset"] = "Page padding"
    };

    private static string PrettyName(string token)
    {
        // TokenLabels is the English fallback; localized catalogs carry ThemeField<token> keys so every
        // editor label follows the user's language instead of staying hardcoded English.
        if (UiText.TryGet("ThemeField" + token, out var localized) && localized.Length > 0) return localized;
        if (TokenLabels.TryGetValue(token, out var friendly)) return friendly;
        var result = System.Text.RegularExpressions.Regex.Replace(token,
            @"(?<!^)([A-Z])", " $1");
        return result.Replace(" Ui ", " UI ").Replace(" Ai ", " AI ");
    }

    private TextBox AddTextField(string label, string initial)
    {
        var field = new TextBox { Text = initial };
        field.SetResourceReference(FrameworkElement.StyleProperty, "SettingsTextBox");
        var row = AddRow(label);
        Grid.SetColumn(field, 1);
        row.Children.Add(field);
        return field;
    }

    /// <summary>Shows "Inter" for a pack URI such as pack://application:,,,/Fonts/#Inter.</summary>
    private static string FriendlyFontName(FontFamily family)
    {
        var source = family.Source;
        var hash = source.LastIndexOf('#');
        if (hash >= 0 && hash < source.Length - 1) return source[(hash + 1)..];
        if (family.FamilyNames.Count > 0) return family.FamilyNames.First().Value;
        return source;
    }

    private void AddFontField(string key, FontFamily family)
    {
        var source = family.Source;
        var field = AddTextField(key, FriendlyFontName(family));
        field.ToolTip = UiText.Get("ThemeFontFieldHelp");
        field.Tag = key;
        field.LostFocus += (_, _) =>
        {
            // Preserve the original pack URI when the user did not change the visible name.
            var text = field.Text.Trim();
            if (text.Length == 0) { field.Text = FriendlyFontName(family); return; }
            var value = string.Equals(text, FriendlyFontName(family), StringComparison.OrdinalIgnoreCase)
                ? source
                : text;
            UpdateToken(key, JsonValue.Create(value));
        };
    }

    private void AddNumberField(string key, double initial)
    {
        var row = AddRow(key);
        if (key is "SettingsBackgroundOpacity" or "DockShadowOpacity")
        {
            var group = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
            var slider = new Slider
            {
                Minimum = key == "SettingsBackgroundOpacity" ? 60 : 0,
                Maximum = 100,
                Value = Math.Clamp(initial * 100, key == "SettingsBackgroundOpacity" ? 60 : 0, 100),
                TickFrequency = 1,
                IsSnapToTickEnabled = true,
                Width = 148,
                VerticalAlignment = VerticalAlignment.Center
            };
            slider.SetResourceReference(FrameworkElement.StyleProperty, "SettingsOpacitySlider");
            var percent = new TextBlock
            {
                Width = 45,
                Margin = new Thickness(9, 0, 0, 0),
                TextAlignment = TextAlignment.Right,
                VerticalAlignment = VerticalAlignment.Center,
                Text = Math.Round(slider.Value).ToString(CultureInfo.InvariantCulture) + "%"
            };
            percent.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            group.Children.Add(slider);
            group.Children.Add(percent);
            Grid.SetColumn(group, 1);
            row.Children.Add(group);
            slider.ValueChanged += (_, _) =>
            {
                percent.Text = Math.Round(slider.Value).ToString(CultureInfo.InvariantCulture) + "%";
                UpdateToken(key, JsonValue.Create(Math.Round(slider.Value) / 100), immediate: false);
            };
            return;
        }

        var field = new TextBox { Text = initial.ToString("0.##", CultureInfo.InvariantCulture) };
        field.SetResourceReference(FrameworkElement.StyleProperty, "SettingsTextBox");
        Grid.SetColumn(field, 1);
        row.Children.Add(field);
        field.LostFocus += (_, _) =>
        {
            if (double.TryParse(field.Text, NumberStyles.Float, CultureInfo.InvariantCulture, out var value)
                && value is >= 0 and <= 2048)
                UpdateToken(key, JsonValue.Create(value));
            else field.Text = initial.ToString("0.##", CultureInfo.InvariantCulture);
        };
    }

    private void AddThicknessField(string key, Thickness initial) =>
        AddFourValueField(key,
            [initial.Left, initial.Top, initial.Right, initial.Bottom]);

    private void AddCornerField(string key, CornerRadius initial) =>
        AddFourValueField(key,
            [initial.TopLeft, initial.TopRight, initial.BottomRight, initial.BottomLeft]);

    private void AddFourValueField(string key, double[] values)
    {
        var allEqual = values.All(v => Math.Abs(v - values[0]) < 0.01);
        var initial = allEqual
            ? values[0].ToString("0.##", CultureInfo.InvariantCulture)
            : string.Join(",", values.Select(v => v.ToString("0.##", CultureInfo.InvariantCulture)));
        var field = AddTextField(key, initial);
        field.ToolTip = UiText.Get("ThemeFourValueFieldHelp");
        field.LostFocus += (_, _) =>
        {
            var parts = field.Text.Split(',', StringSplitOptions.TrimEntries);
            if (parts.Length is not (1 or 4) || !parts.All(part =>
                    double.TryParse(part, NumberStyles.Float, CultureInfo.InvariantCulture, out var number)
                    && number is >= 0 and <= 2048))
            {
                field.Text = initial;
                return;
            }
            // Always persist the full four-value form the theme pack expects.
            var stored = parts.Length == 1
                ? string.Join(",", Enumerable.Repeat(parts[0], 4))
                : field.Text;
            UpdateToken(key, JsonValue.Create(stored));
        };
    }

    private void AddModeField(string key, string initial)
    {
        var row = AddRow("DockBackgroundMode");
        var combo = new ComboBox
        {
            MinHeight = 30,
            ItemsSource = new[]
            {
                new ComboBoxItem { Content = UiText.Get("SurfaceGlass"), Tag = "Glass" },
                new ComboBoxItem { Content = UiText.Get("SurfaceSolid"), Tag = "Solid" },
                new ComboBoxItem { Content = UiText.Get("SurfaceNone"), Tag = "None" }
            }
        };
        var initialMode = initial is "Glass" or "Solid" or "None" ? initial : "Glass";
        combo.SelectedItem = combo.Items.OfType<ComboBoxItem>().First(item => (string)item.Tag == initialMode);
        Grid.SetColumn(combo, 1);
        row.Children.Add(combo);
        combo.SelectionChanged += (_, _) =>
        {
            if ((combo.SelectedItem as ComboBoxItem)?.Tag is string mode)
                UpdateToken(key, JsonValue.Create(mode));
        };
    }

    private void AddBrushField(string key, Brush initial)
    {
        var row = AddRow(key);
        var button = Button("");
        button.HorizontalAlignment = HorizontalAlignment.Stretch;
        button.HorizontalContentAlignment = HorizontalAlignment.Left;
        button.SetResourceReference(System.Windows.Controls.Button.BackgroundProperty, "SettingsControl");
        button.SetResourceReference(System.Windows.Controls.Button.ForegroundProperty, "TextPrimary");
        var content = new StackPanel { Orientation = Orientation.Horizontal };
        var swatch = new Border
        {
            Width = 33,
            Height = 23,
            Background = initial.CloneCurrentValue(),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(5),
            Margin = new Thickness(0, 0, 10, 0)
        };
        swatch.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        content.Children.Add(swatch);
        var title = new TextBlock
        {
            Text = initial is LinearGradientBrush ? UiText.Get("SurfaceGradient") : UiText.Get("SurfaceSolid"),
            VerticalAlignment = VerticalAlignment.Center,
            FontSize = 12
        };
        title.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        content.Children.Add(title);
        button.Content = content;
        Grid.SetColumn(button, 1);
        row.Children.Add(button);
        button.Click += (_, _) =>
        {
            if (Window.GetWindow(this) is not { } owner) return;
            var picker = new ThemeBrushPicker(swatch.Background) { Owner = owner };
            if (picker.ShowDialog() == true && picker.Result is { } brush)
            {
                swatch.Background = brush;
                title.Text = UiText.Get(brush is LinearGradientBrush ? "SurfaceGradient" : "SurfaceSolid");
                UpdateToken(key, BrushToNode(brush));
            }
        };
    }

    private static Button Button(string text)
    {
        var btn = new Button
        {
            Content = text,
            MinHeight = 31,
            Padding = new Thickness(12, 5, 12, 5),
            MinWidth = 96
        };
        btn.SetResourceReference(FrameworkElement.StyleProperty, "SettingsButton");
        return btn;
    }

    private static JsonNode BrushToNode(Brush brush)
    {
        if (brush is SolidColorBrush solid) return JsonValue.Create(solid.Color.ToString())!;
        if (brush is LinearGradientBrush gradient)
        {
            var stops = new JsonArray();
            var positions = new JsonArray();
            foreach (var stop in gradient.GradientStops.OrderBy(s => s.Offset))
            {
                stops.Add(stop.Color.ToString());
                positions.Add(Math.Round(stop.Offset * 100, 3));
            }
            var delta = gradient.EndPoint - gradient.StartPoint;
            var angle = (Math.Atan2(delta.X, -delta.Y) * 180 / Math.PI + 360) % 360;
            return new JsonObject { ["stops"] = stops, ["positions"] = positions, ["angle"] = angle };
        }
        return JsonValue.Create("#FF20232A")!;
    }

    internal object? ResolveTokenValue(string key)
    {
        var resources = Application.Current?.Resources;
        if (resources is null) return null;

        object? fallback = RadiusManager.Dictionary.Contains(key) && ThemePacks.FindOwner(resources, key) is { } owner
            ? owner[key]
            : (resources[key] ?? Application.Current?.TryFindResource(key));
        if (fallback is null || _widgetPack is null) return fallback;

        var sectionName = _mode == AppTheme.Dark ? "dark" : "light";
        var node = FindSectionToken(_widgetPack, sectionName, key)
            ?? FindSectionToken(_widgetPack, "tokens", key);
        if (node is null) return fallback;

        try
        {
            var element = node.Deserialize<JsonElement>();
            return WidgetPack.Convert(element, fallback) ?? fallback;
        }
        catch (JsonException)
        {
            return fallback;
        }
    }

    private static JsonNode? FindSectionToken(JsonObject root, string sectionName, string key)
    {
        var section = root.FirstOrDefault(pair => string.Equals(pair.Key, sectionName, StringComparison.OrdinalIgnoreCase)).Value as JsonObject;
        if (section is null) return null;
        if (section.TryGetPropertyValue(key, out var exact) && exact is not null) return exact;
        return section.FirstOrDefault(pair => string.Equals(pair.Key, key, StringComparison.OrdinalIgnoreCase)).Value;
    }

    internal void UpdateToken(string key, JsonNode? value, bool immediate = true)
    {
        if (_loadingTheme || _widgetPack is null || _id is null) return;
        var section = _mode == AppTheme.Dark ? "dark" : "light";
        var sectionKey = _widgetPack.FirstOrDefault(pair => string.Equals(pair.Key, section, StringComparison.OrdinalIgnoreCase)).Key ?? section;
        if (_widgetPack[sectionKey] is not JsonObject sectionNode)
        {
            sectionNode = new JsonObject();
            _widgetPack[sectionKey] = sectionNode;
        }
        var tokenKey = sectionNode.FirstOrDefault(pair => string.Equals(pair.Key, key, StringComparison.OrdinalIgnoreCase)).Key ?? key;
        sectionNode[tokenKey] = value;
        ScheduleAutoSave(immediate);
    }

    private string? ReadManifest(string name) =>
        _manifest?.FirstOrDefault(property => string.Equals(property.Key, name, StringComparison.OrdinalIgnoreCase)).Value?.ToString();

    private void SetManifest(string name, string value)
    {
        if (_manifest is null) return;
        var key = _manifest.FirstOrDefault(property => string.Equals(property.Key, name, StringComparison.OrdinalIgnoreCase)).Key ?? name;
        _manifest[key] = value;
    }

    private void SaveTheme()
    {
        if (_id is null || _widgetPack is null || _manifest is null) return;
        try
        {
            var pack = ThemePacks.Discover().FirstOrDefault(p => string.Equals(p.Id, _id, StringComparison.OrdinalIgnoreCase));
            if (pack is null || ThemePack.IsBuiltInTheme(pack.Id)) return;

            var name = _name?.Text.Trim() ?? string.Empty;
            if (name.Length is < 1 or > 120)
            {
                _message.Text = UiText.Get("ThemeNameInvalid");
                return;
            }
            SetManifest("Name", name);
            SetManifest("Author", _author?.Text.Trim() ?? string.Empty);
            SetManifest("Description", _description?.Text.Trim() ?? string.Empty);

            var widgetDirectory = Path.Combine(pack.Directory, ThemePack.WidgetFolder);
            Directory.CreateDirectory(widgetDirectory);
            var options = new JsonSerializerOptions { WriteIndented = true };
            AtomicWrite(Path.Combine(pack.Directory, ThemePack.ManifestFileName), _manifest.ToJsonString(options));
            AtomicWrite(Path.Combine(widgetDirectory, WidgetPack.ManifestFileName), _widgetPack.ToJsonString(options));

            ThemePacks.Select(_id, reload: true);
            ThemePacks.ApplyWidgets(_mode);
            RadiusManager.Initialize();
            RadiusManager.Rebuild();
            _message.Text = UiText.Get("ThemeSavedMessage");
            ThemeSaved?.Invoke(this, EventArgs.Empty);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            _message.Text = UiText.Format("ThemeSaveFailed", ex.Message);
        }
    }

    private static void AtomicWrite(string path, string contents)
    {
        var temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.WriteAllText(temp, contents);
            File.Move(temp, path, overwrite: true);
        }
        finally
        {
            if (File.Exists(temp)) File.Delete(temp);
        }
    }
}
