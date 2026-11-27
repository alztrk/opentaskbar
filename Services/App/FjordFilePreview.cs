using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Windows.Data.Pdf;
using Windows.Storage;
using Windows.Storage.Streams;

namespace OpenTaskbar.Services;

internal static class FjordFilePreview
{
    private const int MaxTextBytes = 64 * 1024;
    private static readonly HashSet<string> Images = new(StringComparer.OrdinalIgnoreCase)
        { ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".tif", ".tiff", ".ico" };
    private static readonly HashSet<string> Text = new(StringComparer.OrdinalIgnoreCase)
        { ".txt", ".md", ".json", ".csv", ".log", ".ini", ".xml", ".yaml", ".yml", ".cs", ".py", ".js", ".css", ".html", ".ps1", ".m", ".ts", ".tex" };

    internal static async Task<FrameworkElement> Build(FileEntry? item, CancellationToken token)
    {
        var panel = new StackPanel();
        var muted = new SolidColorBrush(Color.FromRgb(169, 192, 222));
        if (item is null)
        {
            panel.Children.Add(new TextBlock { Text = FjordFilesText.Get("SelectPreview"), TextWrapping = TextWrapping.Wrap, Foreground = muted });
            return panel;
        }

        panel.Children.Add(new TextBlock
        {
            Text = item.Name, FontSize = 19, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap
        });
        var kind = item.IsFolder ? FjordFilesText.Get("Folder") : item.SizeLabel;
        panel.Children.Add(new TextBlock { Text = $"{kind}  ·  {item.DateLabel}",
            Foreground = muted, Margin = new Thickness(0, 4, 0, 13), TextWrapping = TextWrapping.Wrap });
        if (item.IsFolder) return InScrollViewer(panel);

        var extension = Path.GetExtension(item.Name);
        try
        {
            if (Images.Contains(extension))
            {
                var bitmap = await Task.Run(() => ReadImage(item.Path), token);
                token.ThrowIfCancellationRequested();
                panel.Children.Add(Picture(bitmap));
            }
            else if (extension.Equals(".pdf", StringComparison.OrdinalIgnoreCase))
            {
                var (bitmap, pages) = await ReadPdf(item.Path, token);
                token.ThrowIfCancellationRequested();
                panel.Children.Add(new TextBlock { Text = FjordFilesText.Format("PdfPages", pages), Foreground = muted,
                    Margin = new Thickness(0, 0, 0, 9) });
                panel.Children.Add(Picture(bitmap));
            }
            else if (Text.Contains(extension))
            {
                var (value, truncated) = await Task.Run(() => ReadText(item.Path), token);
                token.ThrowIfCancellationRequested();
                if (truncated) panel.Children.Add(new TextBlock { Text = FjordFilesText.Get("TextLimit"), Foreground = muted,
                    Margin = new Thickness(0, 0, 0, 8) });
                var text = new TextBox { Text = value.Length == 0 ? FjordFilesText.Get("EmptyFile") : value,
                    IsReadOnly = true, TextWrapping = TextWrapping.Wrap,
                    FontFamily = new FontFamily("Cascadia Mono, Consolas"), FontSize = 12,
                    Padding = new Thickness(11), Background = new SolidColorBrush(Color.FromRgb(13, 23, 39)),
                    Foreground = Brushes.AliceBlue, BorderThickness = new Thickness(0), MaxHeight = 480,
                    VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
                panel.Children.Add(text);
            }
            else panel.Children.Add(new TextBlock { Text = FjordFilesText.Get("UnsupportedPreview"), Foreground = muted, TextWrapping = TextWrapping.Wrap });
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or InvalidOperationException or COMException)
        {
            panel.Children.Add(new TextBlock { Text = FjordFilesText.Get("PreviewFailed"), Foreground = muted, TextWrapping = TextWrapping.Wrap });
        }
        return InScrollViewer(panel);
    }

    private static ScrollViewer InScrollViewer(StackPanel panel) =>
        new() { Content = panel, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };

    private static Image Picture(BitmapSource bitmap)
    {
        var image = new Image { Source = bitmap, Stretch = Stretch.Uniform,
            MaxHeight = 580, HorizontalAlignment = HorizontalAlignment.Left };
        AutomationProperties.SetName(image, FjordFilesText.Get("FilePreview"));
        return image;
    }

    private static BitmapSource ReadImage(string path)
    {
        using var input = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        return DecodeBitmap(input, 1100);
    }

    private static BitmapSource DecodeBitmap(Stream stream, int decodeWidth)
    {
        var image = new BitmapImage();
        image.BeginInit();
        image.CacheOption = BitmapCacheOption.OnLoad;
        image.DecodePixelWidth = decodeWidth;
        image.StreamSource = stream;
        image.EndInit();
        image.Freeze();
        return image;
    }

    private static (string, bool) ReadText(string path)
    {
        using var input = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        var buffer = new byte[MaxTextBytes];
        var count = input.ReadAtLeast(buffer, buffer.Length, false);
        using var bytes = new MemoryStream(buffer, 0, count, writable: false);
        using var reader = new StreamReader(bytes, Encoding.UTF8, detectEncodingFromByteOrderMarks: true);
        return (reader.ReadToEnd().Replace('\0', '\uFFFD'), input.ReadByte() >= 0);
    }

    private static async Task<(BitmapSource, uint)> ReadPdf(string path, CancellationToken token)
    {
        var file = await StorageFile.GetFileFromPathAsync(path).AsTask(token);
        var pdf = await PdfDocument.LoadFromFileAsync(file).AsTask(token);
        using var page = pdf.GetPage(0);
        using var output = new InMemoryRandomAccessStream();
        await page.RenderToStreamAsync(output, new PdfPageRenderOptions { DestinationWidth = 900 }).AsTask(token);
        output.Seek(0);
        using var reader = new DataReader(output);
        var length = checked((uint)output.Size);
        await reader.LoadAsync(length).AsTask(token);
        var buffer = new byte[length];
        reader.ReadBytes(buffer);
        using var bytes = new MemoryStream(buffer, writable: false);
        return (DecodeBitmap(bytes, 900), pdf.PageCount);
    }
}
