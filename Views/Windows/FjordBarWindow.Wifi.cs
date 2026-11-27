using System;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;

namespace OpenTaskbar;

internal sealed partial class FjordBarWindow
{
    private static Popup CreateWifiPopup(Button button)
    {
        var scan = ButtonFor(FjordBarText.Get("RefreshNetworks"), FjordBarText.Get("ScanNetworks"));
        scan.Foreground = Ink;
        var header = new DockPanel();
        DockPanel.SetDock(scan, Dock.Right);
        header.Children.Add(scan);
        header.Children.Add(Text(FjordBarText.Get("WifiNetworks"), 13, Ink, FontWeights.SemiBold));

        var status = Text(string.Empty, 12, Quiet);
        status.TextWrapping = TextWrapping.Wrap;
        status.Margin = new Thickness(0, 6, 0, 0);
        status.Visibility = Visibility.Collapsed;
        var networks = new ListBox
        {
            MaxHeight = 230, Margin = new Thickness(0, 8, 0, 0),
            Background = Brushes.Transparent, Foreground = Ink, BorderThickness = new Thickness(0)
        };
        AutomationProperties.SetName(networks, FjordBarText.Get("NearbyNetworks"));
        ScrollViewer.SetHorizontalScrollBarVisibility(networks, ScrollBarVisibility.Disabled);
        var password = new PasswordBox
        {
            Margin = new Thickness(0, 8, 0, 0), Padding = new Thickness(5),
            Background = BrushAt("#182B41"), Foreground = Ink, CaretBrush = Ink, BorderBrush = Hairline
        };
        AutomationProperties.SetName(password, FjordBarText.Get("WifiPassword"));
        var passwordLabel = Text(FjordBarText.Get("NetworkPassword"), 12, Quiet);
        passwordLabel.Margin = new Thickness(0, 8, 0, 0);
        var remember = new CheckBox
        {
            Content = FjordBarText.Get("RememberNetwork"), Foreground = Ink, VerticalAlignment = VerticalAlignment.Center
        };
        var connection = ButtonFor(FjordBarText.Get("Connect"), FjordBarText.Get("Connect"));
        connection.HorizontalAlignment = HorizontalAlignment.Right;
        connection.Foreground = Ink;
        connection.Background = BrushAt("#345172");
        var actions = new DockPanel { Margin = new Thickness(0, 8, 0, 0) };
        DockPanel.SetDock(connection, Dock.Right);
        actions.Children.Add(connection);
        actions.Children.Add(remember);
        var content = new StackPanel { Children = { header, status, networks, passwordLabel, password, actions } };
        var surface = new Border
        {
            Width = 320, Background = Panel, BorderBrush = Hairline, BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(10), Padding = new Thickness(12), Child = content
        };
        var popup = new Popup
        {
            PlacementTarget = button, Placement = PlacementMode.Bottom, VerticalOffset = 6,
            AllowsTransparency = true, StaysOpen = false, Child = surface
        };
        CancellationTokenSource? load = null;
        var changingConnection = false;

        WifiNetworks.Network? Selected() => (networks.SelectedItem as ListBoxItem)?.Tag as WifiNetworks.Network;

        void Message(string? message)
        {
            status.Text = message ?? string.Empty;
            status.Visibility = message is null ? Visibility.Collapsed : Visibility.Visible;
        }

        void UpdateControls()
        {
            var selected = Selected();
            networks.IsEnabled = !changingConnection && load is null;
            scan.IsEnabled = networks.IsEnabled;
            networks.Visibility = networks.HasItems ? Visibility.Visible : Visibility.Collapsed;
            actions.Visibility = selected is null ? Visibility.Collapsed : Visibility.Visible;
            var action = selected?.Connected == true ? FjordBarText.Get("Disconnect") : FjordBarText.Get("Connect");
            connection.Content = action;
            connection.ToolTip = action;
            AutomationProperties.SetName(connection, action);
            connection.IsEnabled = networks.IsEnabled && selected is { Connected: true } or { Usable: true };
            passwordLabel.Visibility = password.Visibility = selected is { Connected: false, NeedsPassword: true }
                ? Visibility.Visible : Visibility.Collapsed;
            password.IsEnabled = networks.IsEnabled;
            remember.Visibility = selected is { Connected: false, Usable: true, Profile.Length: 0 }
                ? Visibility.Visible : Visibility.Collapsed;
            remember.IsEnabled = networks.IsEnabled;
        }

        async Task LoadAsync(bool scanNow, string? result = null)
        {
            if (load is not null || changingConnection) return;
            using var request = new CancellationTokenSource();
            load = request;
            var selected = Selected();
            Message(scanNow ? FjordBarText.Get("Scanning") : FjordBarText.Get("Loading"));
            UpdateControls();
            try
            {
                var snapshot = await Task.Run(() => WifiNetworks.ReadAsync(scanNow, request.Token));
                if (request.IsCancellationRequested) return;
                networks.Items.Clear();
                foreach (var network in snapshot.Networks)
                    networks.Items.Add(CreateWifiRow(network));
                var matching = networks.Items.Cast<ListBoxItem>()
                    .Where(item => item.Tag is WifiNetworks.Network network && (selected is null ? network.Connected :
                        network.Ssid == selected.Ssid && network.Secured == selected.Secured &&
                        network.Authentication == selected.Authentication));
                networks.SelectedItem = selected is null ? matching.FirstOrDefault() :
                    matching.FirstOrDefault(item => item.Tag is WifiNetworks.Network network &&
                        network.Adapter == selected.Adapter && network.Profile == selected.Profile) ??
                    matching.FirstOrDefault();
                if (Selected() is null) remember.IsChecked = false;
                Message(result ?? (snapshot.HasAdapter
                    ? snapshot.Networks.Count == 0 ? FjordBarText.Get("NoNetworks") : null
                    : FjordBarText.Get("NoAdapter")));
            }
            catch (OperationCanceledException) when (request.IsCancellationRequested) { }
            catch (Win32Exception error) { Message(WifiError(error)); }
            finally
            {
                if (ReferenceEquals(load, request)) load = null;
                UpdateControls();
            }
        }

        async Task ChangeConnectionAsync()
        {
            if (!connection.IsEnabled || Selected() is not { } network) return;
            var needsPassword = !network.Connected && network.NeedsPassword;
            if (needsPassword && password.Password.Length == 0)
            {
                Message(FjordBarText.Get("EnterPassword"));
                password.Focus();
                return;
            }
            var key = needsPassword ? password.Password : string.Empty;
            var saveNetwork = remember.IsChecked == true;
            password.Clear();
            changingConnection = true;
            Message(network.Connected ? FjordBarText.Get("Disconnecting") : FjordBarText.Get("Connecting"));
            UpdateControls();
            string? message;
            try
            {
                if (network.Connected)
                {
                    var disconnected = await Task.Run(() => WifiNetworks.DisconnectAsync(network));
                    message = disconnected ? FjordBarText.Get("Disconnected") : FjordBarText.Get("DisconnectFailed");
                }
                else
                {
                    var result = await Task.Run(() => WifiNetworks.ConnectAsync(network, key, saveNetwork));
                    message = result switch
                    {
                        WifiNetworks.ConnectionResult.Remembered => FjordBarText.Get("ConnectedAndSaved"),
                        WifiNetworks.ConnectionResult.Connected => FjordBarText.Get("ConnectionEstablished"),
                        WifiNetworks.ConnectionResult.SaveFailed => FjordBarText.Get("SaveFailed"),
                        _ => FjordBarText.Get("ConnectionFailed")
                    };
                }
            }
            catch (ArgumentException error) { message = error.Message; }
            catch (Win32Exception error) { message = WifiError(error); }
            finally
            {
                changingConnection = false;
                UpdateControls();
            }
            if (popup.IsOpen) await LoadAsync(false, message);
        }

        void FocusPassword()
        {
            if (PresentationSource.FromVisual(password) is HwndSource source)
                FjordBarNative.SetForegroundWindow(source.Handle);
            password.Focus();
        }

        networks.SelectionChanged += (_, _) =>
        {
            password.Clear();
            if (load is null && !changingConnection)
            {
                remember.IsChecked = false;
                Message(Selected() is { Connected: false, Usable: false } network
                    ? network.Connectable ? FjordBarText.Get("ProfileRequired") : FjordBarText.Get("NotConnectable")
                    : null);
            }
            UpdateControls();
            if (password.IsVisible && password.IsEnabled)
                password.Dispatcher.BeginInvoke(DispatcherPriority.Input, new Action(() =>
                {
                    if (popup.IsOpen && password.IsVisible && password.IsEnabled)
                        FocusPassword();
                }));
        };
        popup.Opened += async (_, _) =>
        {
            // WPF popups normally opt out of activation. This one needs keyboard input.
            if (PresentationSource.FromVisual(surface) is HwndSource source)
            {
                const int extendedStyle = -20;
                const int noActivate = 0x08000000;
                var style = FjordBarNative.GetWindowLongPtr(source.Handle, extendedStyle).ToInt64();
                FjordBarNative.SetWindowLongPtr(source.Handle, extendedStyle, new nint(style & ~noActivate));
            }
            await LoadAsync(false);
        };
        popup.Closed += (_, _) =>
        {
            var previous = load;
            load = null;
            previous?.Cancel();
            password.Clear();
            networks.Items.Clear();
            Message(null);
        };
        scan.Click += async (_, _) => await LoadAsync(true);
        password.PreviewMouseLeftButtonDown += (_, _) => FocusPassword();
        connection.Click += async (_, _) => await ChangeConnectionAsync();
        surface.PreviewKeyDown += async (_, args) =>
        {
            if (args.Key == Key.Escape)
            {
                popup.IsOpen = false;
                args.Handled = true;
            }
            else if (args.Key == Key.Enter && password.IsKeyboardFocusWithin)
            {
                args.Handled = true;
                await ChangeConnectionAsync();
            }
        };
        button.Click += (_, _) => popup.IsOpen = !popup.IsOpen;
        UpdateControls();
        return popup;
    }

    private static ListBoxItem CreateWifiRow(WifiNetworks.Network network)
    {
        var name = Text(network.Name, 12.5, Ink, FontWeights.SemiBold);
        name.TextTrimming = TextTrimming.CharacterEllipsis;
        var details = network.Connected ? FjordBarText.Get("Connected") : network.Profile.Length > 0 ? FjordBarText.Get("Saved") : network.Security;
        var strength = Text((network.Signal / 100d).ToString("P0", FjordBarText.Culture), 11, Quiet);
        strength.Margin = new Thickness(8, 0, 0, 0);
        var row = new DockPanel();
        DockPanel.SetDock(strength, Dock.Right);
        row.Children.Add(strength);
        row.Children.Add(new StackPanel { Children = { name, Text(details, 11, Quiet) } });
        var item = new ListBoxItem
        {
            Tag = network, Content = row, Padding = new Thickness(7, 4, 7, 4),
            HorizontalContentAlignment = HorizontalAlignment.Stretch
        };
        AutomationProperties.SetName(item, FjordBarText.Format("SignalDescription", network.Name, details, network.Signal));
        return item;
    }

    private static string WifiError(Win32Exception error) => unchecked((uint)error.NativeErrorCode) switch
    {
        5 => FjordBarText.Get("LocationDenied"),
        1062 => FjordBarText.Get("WlanStopped"),
        0x80342002 => FjordBarText.Get("WifiOff"),
        _ => FjordBarText.Format("WindowsError", error.NativeErrorCode)
    };
}
