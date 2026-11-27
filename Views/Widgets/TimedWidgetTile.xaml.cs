using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views.Widgets;

public partial class TimedWidgetTile : UserControl
{
    public static readonly DependencyProperty IsDynamicIslandProperty = DependencyProperty.Register(
        nameof(IsDynamicIsland), typeof(bool), typeof(TimedWidgetTile), new PropertyMetadata(false));
    public static readonly DependencyProperty TitleProperty = DependencyProperty.Register(
        nameof(Title), typeof(string), typeof(TimedWidgetTile), new PropertyMetadata(string.Empty));
    public static readonly RoutedEvent DetailsRequestedEvent = EventManager.RegisterRoutedEvent(
        nameof(DetailsRequested), RoutingStrategy.Bubble, typeof(RoutedEventHandler), typeof(TimedWidgetTile));

    public TimedWidgetTile()
    {
        InitializeComponent();
        Loaded += (_, _) => UpdateTileIcon();
        DataContextChanged += (_, _) => UpdateTileIcon();
    }

    public string Title { get => (string)GetValue(TitleProperty); set => SetValue(TitleProperty, value); }
    public bool IsDynamicIsland { get => (bool)GetValue(IsDynamicIslandProperty); set => SetValue(IsDynamicIslandProperty, value); }
    public event RoutedEventHandler DetailsRequested
    {
        add => AddHandler(DetailsRequestedEvent, value);
        remove => RemoveHandler(DetailsRequestedEvent, value);
    }

    private void UpdateTileIcon()
    {
        if (TileIcon is null) return;
        if (DataContext is CountdownDockWidget)
            TileIcon.Content = FindResource("IconHourglass");
        else
            TileIcon.Content = FindResource("IconTimer");
    }

    private void Details_Click(object sender, RoutedEventArgs e) => RequestDetails();

    private void QuickAction_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is not ITimedDockWidget widget) return;
        if (widget.CanToggle) widget.Toggle();
        else RequestDetails();
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is ITimedDockWidget widget) widget.Reset();
    }

    private void RequestDetails() => RaiseEvent(new RoutedEventArgs(DetailsRequestedEvent, this));
}
