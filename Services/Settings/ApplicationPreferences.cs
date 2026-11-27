using System.Text.Json;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class ApplicationPreferences
{
    private readonly DockSettingsStore _store;
    public DockSettings Current { get; private set; }
    public string? LoadError { get; private set; }
    public event EventHandler? Changed;

    public ApplicationPreferences(DockSettingsStore store)
    {
        _store = store;
        (Current, LoadError) = store.Load();
    }

    public string? Save(DockSettings settings)
    {
        settings = DockProfileService.SyncActive(settings);
        // Read-modify-write: another build may have written Rust-only keys since this
        // snapshot was loaded. Keep those keys instead of overwriting them with stale data.
        settings = MergeFreshExtras(settings, _store.Load().Settings);
        var error = _store.Save(settings);
        if (error is not null) return error;
        Current = settings;
        LoadError = null;
        UiText.Current.SetLanguage(settings.Language);
        ThemeManager.Apply(settings.Theme, settings);
        ThemePacks.Select(settings.ThemePack);
        ThemePacks.ApplyWidgets(settings.Theme);
        Changed?.Invoke(this, EventArgs.Empty);
        return null;
    }

    private static DockSettings MergeFreshExtras(DockSettings current, DockSettings disk)
    {
        // C# does not own these keys: a value read from disk just now wins over the snapshot.
        var root = new Dictionary<string, JsonElement>(current.Extra, StringComparer.Ordinal);
        foreach (var pair in disk.Extra) root[pair.Key] = pair.Value;
        var profiles = current.Profiles?.Select(profile =>
        {
            var fresh = disk.Profiles?.FirstOrDefault(saved =>
                string.Equals(saved.Name, profile.Name, StringComparison.OrdinalIgnoreCase));
            if (fresh is null || fresh.Extra.Count == 0) return profile;
            var merged = new Dictionary<string, JsonElement>(profile.Extra, StringComparer.Ordinal);
            foreach (var pair in fresh.Extra) merged[pair.Key] = pair.Value;
            return profile with { Extra = merged };
        }).ToArray();
        return current with { Extra = root, Profiles = profiles ?? current.Profiles };
    }
}
