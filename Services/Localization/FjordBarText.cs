using System.Globalization;
using System.Resources;

namespace OpenTaskbar;

internal static class FjordBarText
{
    private static readonly ResourceManager Resources = new(
        "OpenTaskbar.FjordBarText", typeof(FjordBarText).Assembly);

    internal static CultureInfo Culture => Services.UiText.Current.Language.Culture;

    internal static string Get(string key) => Resources.GetString(key, Culture) ?? key;

    internal static string Format(string key, params object[] args) =>
        string.Format(Culture, Get(key), args);
}
