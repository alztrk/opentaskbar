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
        _mode = mode;
        _body.Children.Clear();
        _fieldHost = _body;
        _body.Children.Add(_message);

        var pack = ThemePacks.Discover().FirstOrDefault(p => string.Equals(p.Id, id, StringComparison.OrdinalIgnoreCase));
        if (pack is null)
        {
            _message.Text = "Select a theme to manage its properties.";
            return;
        }
        if (ThemePack.IsBuiltInTheme(pack.Id))
        {
            _message.Text = "This is a built-in theme. Duplicate it to edit its colors and layout.";
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

        _message.Text = "Edit the selected theme. Changes are stored in its theme pack.";
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
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(230) });
        var text = new TextBlock { Text = label, FontSize = 13, VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 10, 0), TextWrapping = TextWrapping.Wrap };
        grid.Children.Add(text);
        _fieldHost.Children.Add(grid);
        return grid;
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
        var row = AddRow(key == "SettingsBackgroundOpacity" ? "Window background opacity (0.6–1)" : key);
        var field = new TextBox { Text = initial.ToString("0.##", CultureInfo.InvariantCulture),
            Padding = new Thickness(8, 4, 8, 4), MinHeight = 29 };
        Grid.SetColumn(field, 1);
        row.Children.Add(field);
        field.LostFocus += (_, _) =>
        {
            if (double.TryParse(field.Text, NumberStyles.Float, CultureInfo.InvariantCulture, out var value)
                && value >= (key == "SettingsBackgroundOpacity" ? 0.6 : 0) && value <= (key is "SettingsBackgroundOpacity" or "DockShadowOpacity" ? 1 : 2048))
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
        var button = Button("Edit color / gradient");
        button.HorizontalAlignment = HorizontalAlignment.Stretch;
        button.HorizontalContentAlignment = HorizontalAlignment.Left;
        button.Background = initial.CloneCurrentValue();
        button.Foreground = (Brush)Application.Current.Resources["TextPrimary"];
        Grid.SetColumn(button, 1);
        row.Children.Add(button);
        button.Click += (_, _) =>
        {
            if (Window.GetWindow(this) is not { } owner) return;
            var picker = new ThemeBrushPicker(button.Background)
            {
                Owner = owner
            };
            if (picker.ShowDialog() == true && picker.Result is { } brush)
            {
                button.Background = brush;
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
