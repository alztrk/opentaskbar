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
                    "StrokeFocus", "SurfaceSelected", "SurfaceHover"]),
        ("Dock background", ["DockBackgroundMode", "DockMaterial", "DockBorderMaterial", "DockHighlight"]),
        ("Dock geometry", ["DockRadius", "DockInnerRadius", "DockWidgetPadding", "DockShadowBlur", "DockShadowOpacity", "DockShadowColor"]),
        ("Widgets", ["WidgetCard", "WidgetCardStroke", "WidgetTrack", "WidgetAccent",
                     "DockWidgetSurface", "DockWidgetRadius"]),
        ("Settings window", ["SettingsBackground", "SettingsBackgroundOpacity", "SettingsControl", "SettingsControlHover", "SettingsSidebarOverlay"]),
        ("Typography and spacing", ["InterfaceFont", "TextSizeBody", "TextSizeCaption",
                                    "ControlRadius", "PopupRadius", "SettingsPageInset"])
    ];

    private readonly StackPanel _body = new();
    private Panel _fieldHost;
    private readonly TextBlock _message = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 10, 0, 14) };
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
    }

    public void LoadTheme(string? id, AppTheme mode)
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
            ShowCurrentAppearance(pack?.DisplayName ?? "Windows appearance");
            return;
        }

        try
        {
            var manifestPath = Path.Combine(pack.Directory, ThemePack.ManifestFileName);
            _manifest = JsonNode.Parse(File.ReadAllText(manifestPath)) as JsonObject ?? new JsonObject();
            var widgetPath = Path.Combine(pack.Directory, ThemePack.WidgetFolder, WidgetPack.ManifestFileName);
            _widgetPack = File.Exists(widgetPath)
                ? JsonNode.Parse(File.ReadAllText(widgetPath)) as JsonObject ?? new JsonObject()
                : new JsonObject { ["name"] = pack.DisplayName + " Widgets" };
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            _message.Text = "Unable to load theme: " + ex.Message;
            return;
        }

        _message.Text = "Editing " + pack.DisplayName + " · Changes are saved to the theme pack.";
        ShowColorSwatches();
        SectionHeader("Theme information");
        _name = AddTextField("Name", ReadManifest("Name") ?? pack.DisplayName);
        _author = AddTextField("Author", ReadManifest("Author") ?? string.Empty);
        _description = AddTextField("Description", ReadManifest("Description") ?? string.Empty);
        foreach (var (title, keys) in Sections)
        {
            SectionHeader(title);
            foreach (var key in keys)
            {
                var existing = Application.Current.Resources[key];
                if (existing is null) continue;
                if (key == "DockBackgroundMode") AddModeField(key, existing.ToString() ?? "Glass");
                else if (existing is Brush brush) AddBrushField(key, brush);
                else if (existing is CornerRadius radius) AddNumberField(key, radius.TopLeft);
                else if (existing is Thickness thickness) AddNumberField(key, thickness.Left);
                else if (existing is double value) AddNumberField(key, value);
                else if (existing is FontFamily family) AddTextTokenField(key, family.Source);
            }
        }

        var covered = Sections.SelectMany(section => section.Keys).ToHashSet(StringComparer.Ordinal);
        var advanced = new Expander
        {
            Header = "Advanced theme tokens",
            IsExpanded = false,
            Margin = new Thickness(0, 20, 0, 0)
        };
        var advancedPanel = new StackPanel { Margin = new Thickness(0, 10, 0, 0) };
        advanced.Content = advancedPanel;
        _body.Children.Add(advanced);
        _fieldHost = advancedPanel;
        foreach (var key in AllThemeKeys(Application.Current.Resources)
                     .Where(key => !covered.Contains(key))
                     .Distinct(StringComparer.Ordinal)
                     .OrderBy(key => key, StringComparer.OrdinalIgnoreCase))
        {
            var existing = Application.Current.TryFindResource(key);
            if (existing is Brush brush) AddBrushField(key, brush);
            else if (existing is Color color) AddBrushField(key, new SolidColorBrush(color));
            else if (existing is CornerRadius radius) AddNumberField(key, radius.TopLeft);
            else if (existing is Thickness thickness) AddNumberField(key, thickness.Left);
            else if (existing is double number) AddNumberField(key, number);
            else if (existing is FontFamily family) AddTextTokenField(key, family.Source);
        }
        _fieldHost = _body;

        var actions = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 22, 0, 18) };
        var save = Button("Save theme");
        save.Click += (_, _) => SaveTheme();
        actions.Children.Add(save);
        _body.Children.Add(actions);
    }

    private static readonly (string Label, string Key)[] ColorOverview =
    [
        ("Accent", "AccentBrush"),
        ("Background", "SettingsBackground"),
        ("Surface", "SettingsControl"),
        ("Text", "TextPrimary"),
        ("Secondary", "TextSecondary"),
        ("Border", "StrokeSubtle")
    ];

    private void ShowColorSwatches()
    {
        SectionHeader("Color palette");
        var strip = new WrapPanel { Margin = new Thickness(0, 0, 0, 3) };
        foreach (var (label, key) in ColorOverview)
        {
            if (Application.Current.TryFindResource(key) is not Brush brush) continue;
            var cell = new StackPanel { Width = 92, Margin = new Thickness(0, 0, 10, 6) };
            cell.Children.Add(new Border
            {
                Background = brush.CloneCurrentValue(),
                Height = 34, CornerRadius = new CornerRadius(6),
                BorderThickness = new Thickness(1),
                BorderBrush = Application.Current.TryFindResource("StrokeSubtle") as Brush
            });
            var caption = new TextBlock { Text = label, FontSize = 11, Margin = new Thickness(0, 7, 0, 0) };
            caption.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            cell.Children.Add(caption);
            strip.Children.Add(cell);
        }
        _body.Children.Add(strip);
    }

    private void ShowCurrentAppearance(string title)
    {
        _message.Text = title + " · Windows colors are used until you choose a custom theme.";
        ShowColorSwatches();
        SectionHeader("Dock background");
        var dock = new Border
        {
            CornerRadius = new CornerRadius(12),
            Height = 52,
            MaxWidth = 350,
            HorizontalAlignment = HorizontalAlignment.Left,
            Padding = new Thickness(18, 10, 18, 10),
            Margin = new Thickness(0, 0, 0, 12),
            Background = Application.Current.TryFindResource("DockMaterial") as Brush
                ?? Application.Current.TryFindResource("SettingsControl") as Brush
        };
        var caption = new TextBlock { Text = "Dock · Glass / Solid / None", VerticalAlignment = VerticalAlignment.Center };
        caption.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        dock.Child = caption;
        _body.Children.Add(dock);

        SectionHeader("Customize appearance");
        var prompt = new TextBlock
        {
            Text = "Create an editable copy to change colors, gradients, fonts, widget spacing and window transparency.",
            TextWrapping = TextWrapping.Wrap,
            Margin = new Thickness(0, 0, 0, 12)
        };
        prompt.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        _body.Children.Add(prompt);
        var create = Button("Create editable theme");
        create.HorizontalAlignment = HorizontalAlignment.Left;
        create.Click += (_, _) =>
        {
            try
            {
                var id = ThemePackTransfer.CreateTheme("My " + (_mode == AppTheme.Light ? "Light" : "Dark") + " Theme",
                    null, _id);
                ThemeCreated?.Invoke(this, id);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
            {
                _message.Text = "Theme creation failed: " + ex.Message;
            }
        };
        _body.Children.Add(create);
    }

    private static IEnumerable<string> AllThemeKeys(ResourceDictionary dictionary)
    {
        foreach (var key in dictionary.Keys)
            if (key is string text) yield return text;
        foreach (var child in dictionary.MergedDictionaries)
            foreach (var key in AllThemeKeys(child)) yield return key;
    }

    private void SectionHeader(string title)
    {
        var stack = new StackPanel { Margin = new Thickness(0, 22, 0, 9) };
        stack.Children.Add(new TextBlock { Text = title, FontWeight = FontWeights.SemiBold, FontSize = 14,
            Margin = new Thickness(0, 0, 0, 9) });
        var divider = new Border { Height = 1 };
        divider.SetResourceReference(Border.BackgroundProperty, "StrokeSubtle");
        stack.Children.Add(divider);
        _body.Children.Add(stack);
    }

    private Grid AddRow(string label)
    {
        var grid = new Grid { MinHeight = 43, Margin = new Thickness(0, 0, 0, 2) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(204) });
        var text = new TextBlock { Text = PrettyName(label), FontSize = 13, VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 10, 0), TextWrapping = TextWrapping.Wrap };
        grid.Children.Add(text);
        _fieldHost.Children.Add(grid);
        return grid;
    }

    private static string PrettyName(string token)
    {
        if (token == "SettingsBackgroundOpacity") return "Settings background opacity";
        if (token == "DockBackgroundMode") return "Dock material";
        var result = System.Text.RegularExpressions.Regex.Replace(token,
            @"(?<!^)([A-Z])", " $1");
        return result.Replace(" Ui ", " UI ").Replace(" Ai ", " AI ");
    }

    private TextBox AddTextField(string label, string initial)
    {
        var field = new TextBox { Text = initial, MinHeight = 29, Padding = new Thickness(8, 4, 8, 4),
            VerticalContentAlignment = VerticalAlignment.Center };
        var row = AddRow(label);
        Grid.SetColumn(field, 1);
        row.Children.Add(field);
        return field;
    }

    private void AddTextTokenField(string key, string initial)
    {
        var field = AddTextField(key, initial);
        field.Tag = key;
        field.LostFocus += (_, _) => UpdateToken(key, JsonValue.Create(field.Text));
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
            var percent = new TextBlock { Width = 45, Margin = new Thickness(9, 0, 0, 0),
                TextAlignment = TextAlignment.Right, VerticalAlignment = VerticalAlignment.Center,
                Text = Math.Round(slider.Value).ToString(CultureInfo.InvariantCulture) + "%" };
            percent.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            group.Children.Add(slider);
            group.Children.Add(percent);
            Grid.SetColumn(group, 1);
            row.Children.Add(group);
            slider.ValueChanged += (_, _) =>
            {
                percent.Text = Math.Round(slider.Value).ToString(CultureInfo.InvariantCulture) + "%";
                UpdateToken(key, JsonValue.Create(Math.Round(slider.Value) / 100));
            };
            return;
        }

        var field = new TextBox { Text = initial.ToString("0.##", CultureInfo.InvariantCulture),
            Padding = new Thickness(8, 4, 8, 4), MinHeight = 29 };
        field.SetResourceReference(TextBox.ForegroundProperty, "TextPrimary");
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

    private void AddModeField(string key, string initial)
    {
        var row = AddRow("Dock background");
        var combo = new ComboBox { MinHeight = 30, ItemsSource = new[] { "Glass", "Solid", "None" } };
        combo.SelectedItem = initial is "Glass" or "Solid" or "None" ? initial : "Glass";
        Grid.SetColumn(combo, 1);
        row.Children.Add(combo);
        combo.SelectionChanged += (_, _) =>
        {
            if (combo.SelectedItem is string mode)
                UpdateToken(key, JsonValue.Create(mode));
        };
    }

    private void AddBrushField(string key, Brush initial)
    {
        var row = AddRow(key);
        var button = Button("");
        button.HorizontalAlignment = HorizontalAlignment.Stretch;
        button.HorizontalContentAlignment = HorizontalAlignment.Left;
        button.SetResourceReference(Button.BackgroundProperty, "SettingsControl");
        button.SetResourceReference(Button.ForegroundProperty, "TextPrimary");
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
        var title = new TextBlock { Text = initial is LinearGradientBrush ? "Gradient" : "Solid",
            VerticalAlignment = VerticalAlignment.Center, FontSize = 12 };
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
                title.Text = brush is LinearGradientBrush ? "Gradient" : "Solid";
                UpdateToken(key, BrushToNode(brush));
            }
        };
    }

    private static Button Button(string text) => new()
    {
        Content = text, MinHeight = 31, Padding = new Thickness(10, 5, 10, 5), MinWidth = 108
    };

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

    private void UpdateToken(string key, JsonNode? value)
    {
        if (_widgetPack is null || _id is null) return;
        var section = _mode == AppTheme.Dark ? "dark" : "light";
        if (_widgetPack[section] is not JsonObject sectionNode)
        {
            sectionNode = new JsonObject();
            _widgetPack[section] = sectionNode;
        }
        sectionNode[key] = value;
        _message.Text = "Unsaved changes";
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
            var pack = ThemePacks.Discover().FirstOrDefault(p => p.Id == _id);
            if (pack is null || ThemePack.IsBuiltInTheme(pack.Id)) return;

            var name = _name?.Text.Trim() ?? string.Empty;
            if (name.Length is < 1 or > 120)
            {
                _message.Text = "Theme name must contain 1–120 characters.";
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
            _message.Text = "Theme saved.";
            ThemeSaved?.Invoke(this, EventArgs.Empty);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException)
        {
            _message.Text = "Unable to save theme: " + ex.Message;
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
