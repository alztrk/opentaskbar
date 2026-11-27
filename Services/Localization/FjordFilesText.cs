using System.Globalization;
using System.Resources;
using System.Windows.Data;
using System.Windows.Markup;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

internal static class FjordFilesText
{
    private static readonly ResourceManager Resources = new(
        "OpenTaskbar.Services.FjordFilesText", typeof(FjordFilesText).Assembly);

    internal static CultureInfo Culture => UiText.Current.Language.Culture;

    internal static string Get(string key, CultureInfo? culture = null) =>
        Resources.GetString(key, culture ?? Culture) ?? key;

    internal static string Format(string key, params object[] args) =>
        string.Format(Culture, Get(key), args);
}

public sealed class FjordLocExtension(string key) : MarkupExtension
{
    public override object ProvideValue(IServiceProvider serviceProvider) =>
        new Binding(nameof(UiText.Language))
        {
            Source = UiText.Current,
            Mode = BindingMode.OneWay,
            Converter = new ResourceConverter(key)
        }.ProvideValue(serviceProvider);

    private sealed class ResourceConverter(string key) : IValueConverter
    {
        public object Convert(object value, Type targetType, object parameter, CultureInfo culture) =>
            FjordFilesText.Get(key, value is AppLanguage language ? language.Culture : null);

        public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) =>
            throw new NotSupportedException();
    }
}
