using System.Text.Json;

namespace OpenTaskbar.Services;

/// <summary>
/// Global rendering preference kept outside the profile-aware settings file so Program.Main can read it
/// before any WPF visual tree exists. Software rendering saves roughly 100 MB of private memory but makes
/// magnification and flyouts less fluid; the choice takes effect at the next start.
/// </summary>
internal static class RenderModePreference
{
    internal const string FileName = "render-mode.json";
    private static readonly string Path = System.IO.Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "OpenTaskbar", FileName);

    /// <summary>Shared `%LOCALAPPDATA%\OpenTaskbar\render-mode.json`, also read by the Rust port.</summary>
    internal static string PreferencePath => Path;

    /// <summary>Reads the shared wire form <c>{"software":bool}</c> from any path (Rust writes the same bytes).</summary>
    internal static bool IsSoftwareFrom(string path)
    {
        try
        {
            if (!System.IO.File.Exists(path)) return false;
            using var document = JsonDocument.Parse(System.IO.File.ReadAllText(path));
            return document.RootElement.TryGetProperty("software", out var value)
                && value.ValueKind == JsonValueKind.True;
        }
        catch
        {
            return false;
        }
    }

    public static bool IsSoftware() => IsSoftwareFrom(Path);

    /// <summary>Writes exactly <c>{"software":true|false}</c> with no BOM, matching Rust `set_software`.</summary>
    internal static void SetSoftwareTo(string path, bool value)
    {
        try
        {
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.WriteAllText(path, JsonSerializer.Serialize(new { software = value }),
                new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Render mode preference could not be saved: {ex.Message}");
        }
    }

    public static void SetSoftware(bool value) => SetSoftwareTo(Path, value);
}
