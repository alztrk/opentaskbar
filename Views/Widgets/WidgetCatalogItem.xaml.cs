using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Models;

namespace OpenTaskbar.Views.Widgets;

public partial class WidgetCatalogItem : UserControl
{
    public static readonly RoutedEvent InstallationRequestedEvent = EventManager.RegisterRoutedEvent(
        nameof(InstallationRequested), RoutingStrategy.Bubble, typeof(RoutedEventHandler), typeof(WidgetCatalogItem));
    public static readonly RoutedEvent SettingsRequestedEvent = EventManager.RegisterRoutedEvent(
        nameof(SettingsRequested), RoutingStrategy.Bubble, typeof(RoutedEventHandler), typeof(WidgetCatalogItem));

    public WidgetCatalogItem() => InitializeComponent();

    public event RoutedEventHandler InstallationRequested
    {
        add => AddHandler(InstallationRequestedEvent, value);
        remove => RemoveHandler(InstallationRequestedEvent, value);
    }

    public event RoutedEventHandler SettingsRequested
    {
        add => AddHandler(SettingsRequestedEvent, value);
        remove => RemoveHandler(SettingsRequestedEvent, value);
    }

    private void Preview_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is DockWidgetOption option && option.IsHardwareAvailable)
            RaiseEvent(new RoutedEventArgs(SettingsRequestedEvent, this));
    }

    private void Install_Click(object sender, RoutedEventArgs e)
    {
        e.Handled = true;
        if (DataContext is DockWidgetOption)
            RaiseEvent(new RoutedEventArgs(InstallationRequestedEvent, this));
    }

    private void FlyoutPreview_Click(object sender, RoutedEventArgs e)
    {
        e.Handled = true;
        if (DataContext is DockWidgetOption option) option.ShowFlyoutPreview = !option.ShowFlyoutPreview;
    }

    private void Close_Click(object sender, RoutedEventArgs e)
    {
        e.Handled = true;
        if (DataContext is DockWidgetOption option)
        {
            option.IsSettingsExpanded = false;
        }
    }
}
