using System.ComponentModel;
using System.Globalization;
using System.Windows.Data;
using System.Windows.Markup;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class UiText : INotifyPropertyChanged
{
    private AppLanguage _language = LanguageCatalog.FromCulture(CultureInfo.CurrentUICulture);

    public AppLanguage Language => _language;
    public bool IsEnglish => string.Equals(_language.Culture.TwoLetterISOLanguageName, "en", StringComparison.OrdinalIgnoreCase);
    public static IReadOnlyList<AppLanguage> Languages => LanguageCatalog.Languages
        .Append(Current._language)
        .Distinct()
        .OrderBy(language => language.Culture.EnglishName, StringComparer.OrdinalIgnoreCase)
        .ToArray();
    public static UiText Current { get; } = new();
    public event PropertyChangedEventHandler? PropertyChanged;

    public string this[string key] => LanguageCatalog.Get(key, _language);

    public static string Get(string key) => Current[key];

    /// <summary>Non-throwing lookup for dynamically composed keys; returns false when no catalog has the key.</summary>
    public static bool TryGet(string key, out string value) =>
        LanguageCatalog.TryGet(key, Current._language, out value);

    public static IReadOnlyCollection<string> GetTranslations(string key) => LanguageCatalog.GetTranslations(key);

    public static string Format(string key, params object[] values) =>
        string.Format(Current._language.Culture, Get(key), values);

    public void SetLanguage(AppLanguage language)
    {
        if (!LanguageCatalog.IsSupported(language))
            throw new ArgumentException($"Language '{language.CultureName}' is not supported.", nameof(language));
        CultureInfo.DefaultThreadCurrentCulture = language.Culture;
        CultureInfo.DefaultThreadCurrentUICulture = language.Culture;
        CultureInfo.CurrentCulture = language.Culture;
        CultureInfo.CurrentUICulture = language.Culture;
        if (_language == language) return;
        _language = language;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs("Item[]"));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Language)));
    }
}

[MarkupExtensionReturnType(typeof(string))]
public sealed class LocExtension(string key) : MarkupExtension
{
    public override object ProvideValue(IServiceProvider serviceProvider) =>
        new Binding($"[{key}]") { Source = UiText.Current, Mode = BindingMode.OneWay }.ProvideValue(serviceProvider);
}
