using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>Application groups: a single dock icon whose flyout lists the member apps.</summary>
public partial class MainWindow
{
    private DockFlyout? _groupFlyout;
    private DockAppGroup? _draggedGroup;
    private Point _dragGroupStartPoint;

    /// <summary>Lightweight display model for a group icon (icon, name, live member count).</summary>
    private sealed class GroupViewModel(DockAppGroup group) : INotifyPropertyChanged
    {
        public string Id { get; } = group.Id;
        public string Name { get; } = group.Name;
        public string Initial => string.IsNullOrWhiteSpace(Name) ? "?" : Name.Trim()[0].ToString().ToUpperInvariant();
        public int MemberCount => group.ExecutablePaths.Count;
        public BitmapSource? Icon { get; set; }
        public event PropertyChangedEventHandler? PropertyChanged;
        public void Refresh() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(MemberCount)));
        public void SetIcon(BitmapSource? icon)
        {
            Icon = icon;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Icon)));
        }
    }

    private IReadOnlyList<DockAppGroup> AppGroups => _preferences.Current.AppGroups ?? [];
    private DockAppGroup? FindAppGroup(string id) => AppGroups.FirstOrDefault(g =>
        string.Equals(g.Id, id, StringComparison.OrdinalIgnoreCase));
    private DockAppGroup? GroupContaining(string executablePath) => AppGroups.FirstOrDefault(g =>
        g.ExecutablePaths.Contains(executablePath, StringComparer.OrdinalIgnoreCase));

    private DockAppGroup? GroupContaining(DockApp app) =>
        GroupContaining(app.DockKey)
        ?? (string.IsNullOrWhiteSpace(app.LaunchArguments) ? GroupContaining(app.ExecutablePath) : null);

    private static bool SamePath(string? a, string? b) =>
        string.Equals(a, b, StringComparison.OrdinalIgnoreCase);

    private GroupViewModel CreateGroupViewModel(DockAppGroup group)
    {
        var vm = new GroupViewModel(group);
        // Use the first member's icon (or resolve asynchronously from its executable if not pinned/loaded yet).
        foreach (var path in group.ExecutablePaths)
        {
            var app = FindDockApp(path);
            if (app?.Icon is not null) { vm.Icon = app.Icon; break; }
        }
        if (vm.Icon is null && group.ExecutablePaths.Count > 0)
        {
            var firstPath = group.ExecutablePaths[0];
            _ = RunningAppIconCache.IconLoaderThread.Run(() => ApplicationIconProvider.GetForFile(firstPath))
                .ContinueWith(task =>
                {
                    if (!task.IsCompletedSuccessfully || task.Result is not { } raw) return;
                    Dispatcher.BeginInvoke(new Action(() =>
                    {
                        if (vm.Icon is null) vm.SetIcon(ThemePacks.ApplyIcon(raw, firstPath));
                    }));
                }, TaskScheduler.Default);
        }
        return vm;
    }

    // ---- Group creation / editing ----

    private void SaveGroups(IReadOnlyList<DockAppGroup>? groups)
    {
        var error = _preferences.Save(_preferences.Current with
        {
            AppGroups = groups is { Count: > 0 } ? groups : null
        });
        if (error is not null) { ReportError(error); return; }
        _groupFlyout?.Close(restoreFocus: false);
        RebuildDockOrder();
        ResizeDockToContent();
        _replacementSession?.RefreshPosition();
    }

    private static string NewGroupId() => Guid.NewGuid().ToString("N");

    private void NewGroup_Click(object sender, RoutedEventArgs e)
    {
        var name = TextPrompt.Ask(UiText.Get("NewGroup"), UiText.Get("GroupNamePrompt"));
        if (string.IsNullOrWhiteSpace(name)) return;
        name = name.Trim();
        if (AppGroups.Any(g => SamePath(g.Name, name)))
        {
            ReportError("ErrorGroupExists");
            return;
        }
        var groups = AppGroups.ToList();
        groups.Add(new DockAppGroup(NewGroupId(), name, []));
        SaveGroups(groups);
    }

    private void RenameGroup_Click(object sender, RoutedEventArgs e)
    {
        if (_contextTargetElement?.DataContext is not GroupViewModel vm) return;
        var group = FindAppGroup(vm.Id);
        if (group is null) return;
        var name = TextPrompt.Ask(UiText.Get("RenameGroup"), UiText.Get("GroupNamePrompt"), group.Name);
        if (string.IsNullOrWhiteSpace(name)) return;
        name = name.Trim();
        if (AppGroups.Any(g => !SamePath(g.Id, group.Id) && SamePath(g.Name, name)))
        {
            ReportError("ErrorGroupExists");
            return;
        }
        SaveGroups(AppGroups.Select(g => SamePath(g.Id, group.Id) ? g with { Name = name } : g).ToList());
    }

    private void DeleteGroup_Click(object sender, RoutedEventArgs e)
    {
        if (_contextTargetElement?.DataContext is not GroupViewModel vm) return;
        var group = FindAppGroup(vm.Id);
        if (group is null) return;
        // Members return to the dock as individual app icons; DockItemOrder keeps their old tokens harmless.
        SaveGroups(AppGroups.Where(g => !SamePath(g.Id, group.Id)).ToList());
    }

    private void PopulateAddToGroupMenu(MenuItem menu, DockApp? app)
    {
        menu.Items.Clear();
        if (app is null) return;
        var current = GroupContaining(app);

        foreach (var group in AppGroups)
        {
            var item = new MenuItem
            {
                Header = group.Name.Replace("_", "__"),
                IsCheckable = true,
                IsChecked = current is not null && SamePath(current.Id, group.Id)
            };
            var captured = group;
            item.Click += (_, _) => ToggleAppInGroup(app, captured);
            menu.Items.Add(item);
        }
        menu.Items.Add(new Separator());
        var create = new MenuItem { Header = UiText.Get("NewGroup"), Icon = FindResource("IconPlus") };
        create.Click += (_, _) => CreateGroupWithApp(app);
        menu.Items.Add(create);
    }

    private void CreateGroupWithApp(DockApp app)
    {
        var name = TextPrompt.Ask(UiText.Get("NewGroup"), UiText.Get("GroupNamePrompt"));
        if (string.IsNullOrWhiteSpace(name)) return;
        name = name.Trim();
        if (AppGroups.Any(g => SamePath(g.Name, name)))
        {
            ReportError("ErrorGroupExists");
            return;
        }
        RemoveAppFromItsGroup(app, save: false);
        var groups = AppGroups.ToList();
        groups.Add(new DockAppGroup(NewGroupId(), name, [app.DockKey]));
        SaveGroups(groups);
    }

    /// <summary>Adds or removes one app; an app can belong to at most one group.</summary>
    private void ToggleAppInGroup(DockApp app, DockAppGroup group)
    {
        if (GroupContaining(app) is { } current && SamePath(current.Id, group.Id))
        {
            RemoveAppFromItsGroup(app, save: false);
        }
        else
        {
            RemoveAppFromItsGroup(app, save: false, preserveGroupId: group.Id);
            var groups = AppGroups.Select(g => SamePath(g.Id, group.Id)
                ? g with { ExecutablePaths = g.ExecutablePaths.Append(app.DockKey).Distinct(StringComparer.OrdinalIgnoreCase).ToList() }
                : g).ToList();
            SaveGroups(groups);
            return;
        }
        SaveGroups(AppGroups.ToList());
    }

    private void RemoveAppFromItsGroup(DockApp app, bool save = true, string? preserveGroupId = null)
    {
        RemoveAppFromItsGroup(app.DockKey, save: false, preserveGroupId: preserveGroupId);
        if (string.IsNullOrWhiteSpace(app.LaunchArguments) && !SamePath(app.DockKey, app.ExecutablePath))
        {
            RemoveAppFromItsGroup(app.ExecutablePath, save: false, preserveGroupId: preserveGroupId);
        }
        if (save) SaveGroups(AppGroups.ToList());
    }

    /// <summary>
    /// Removes <paramref name="executablePath"/> from whichever group contains it, dropping that group only if
    /// removing the app made it empty. Pre-existing empty groups (e.g. a newly created group awaiting its first app)
    /// are preserved.
    /// </summary>
    internal static List<DockAppGroup> RemoveAppFromGroups(
        IReadOnlyList<DockAppGroup> groups,
        string executablePath,
        string? preserveGroupId = null)
    {
        var result = new List<DockAppGroup>(groups.Count);
        foreach (var group in groups)
        {
            if (!group.ExecutablePaths.Contains(executablePath, StringComparer.OrdinalIgnoreCase))
            {
                result.Add(group);
                continue;
            }

            var remaining = group.ExecutablePaths.Where(p => !SamePath(p, executablePath)).ToList();
            if (remaining.Count > 0 || (preserveGroupId is not null && SamePath(group.Id, preserveGroupId)))
            {
                result.Add(group with { ExecutablePaths = remaining });
            }
        }
        return result;
    }

    private void RemoveAppFromItsGroup(string executablePath, bool save = true, string? preserveGroupId = null)
    {
        var groups = RemoveAppFromGroups(AppGroups, executablePath, preserveGroupId);
        if (save) SaveGroups(groups);
        else PersistGroupsWithoutRebuild(groups);
    }

    private void PersistGroupsWithoutRebuild(IReadOnlyList<DockAppGroup> groups)
    {
        var error = _preferences.Save(_preferences.Current with
        {
            AppGroups = groups is { Count: > 0 } ? groups : null
        });
        if (error is not null) ReportError(error);
    }

    private void RemoveAppFromGroupViaFlyout(DockAppGroup group, string executablePath)
    {
        var groups = AppGroups.Select(g => SamePath(g.Id, group.Id)
            ? g with { ExecutablePaths = g.ExecutablePaths.Where(p => !SamePath(p, executablePath)).ToList() }
            : g).Where(g => !SamePath(g.Id, group.Id) || g.ExecutablePaths.Count > 0).ToList();
        // Only the group that just became empty is pruned; other empty groups are preserved.
        SaveGroups(groups);
    }

    // ---- Group flyout ----

    private void DockGroup_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { DataContext: GroupViewModel vm }) return;
        var group = FindAppGroup(vm.Id);
        if (group is null) return;
        _groupFlyout ??= new DockFlyout();
        _groupFlyout.Toggle(sender as UIElement ?? this, () => BuildGroupFlyoutContent(group));
    }

    private UIElement BuildGroupFlyoutContent(DockAppGroup group)
    {
        var panel = new StackPanel { Width = 240 };
        panel.Children.Add(DockFlyout.Header(group.Name, bottomMargin: 6));
        if (group.ExecutablePaths.Count == 0)
        {
            panel.Children.Add(DockFlyout.Caption(UiText.Get("GroupEmptyHint")));
            return panel;
        }

        foreach (var path in group.ExecutablePaths)
        {
            var app = FindDockApp(path);
            var name = app?.Name ?? System.IO.Path.GetFileNameWithoutExtension(path);
            var row = new DockPanel { LastChildFill = true, Margin = new Thickness(0, 0, 0, 2) };

            var icon = new Image
            {
                Source = app?.Icon ?? ApplicationIconProvider.GetForFile(path),
                Width = 22,
                Height = 22,
                Stretch = Stretch.Uniform,
                Margin = new Thickness(2, 0, 10, 0)
            };
            DockPanel.SetDock(icon, Dock.Left);
            row.Children.Add(icon);

            var label = new TextBlock { Text = name, VerticalAlignment = VerticalAlignment.Center };
            label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            row.Children.Add(label);

            var menu = new ContextMenu();
            var remove = new MenuItem { Header = UiText.Get("RemoveFromGroup"), Icon = FindResource("IconClose") };
            remove.SetResourceReference(StyleProperty, "DestructiveMenuItem");
            var capturedPath = path;
            remove.Click += (_, _) => RemoveAppFromGroupViaFlyout(group, capturedPath);
            menu.Items.Add(remove);
            row.ContextMenu = menu;

            var capturedPathForLaunch = path;
            var button = DockFlyout.FlatButton(row, (sender, _) =>
            {
                var targetApp = app ?? new DockApp(name, capturedPathForLaunch);
                if (sender is not FrameworkElement anchor || !TryShowWindowPicker(targetApp, anchor))
                    ActivateDockApp(targetApp);
                _groupFlyout?.Close(restoreFocus: false);
            }, name);
            button.HorizontalContentAlignment = HorizontalAlignment.Left;
            panel.Children.Add(button);
        }
        panel.Children.Add(DockFlyout.Caption(UiText.Get("GroupDropHint")));
        return panel;
    }

    private void ActivateDockApp(DockApp app)
    {
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        OpenDockApp(app, toggleIfForeground: true, _foregroundAtAppPress);
        _foregroundAtAppPress = null;
    }

    // ---- Group drag & drop ----

    private void DockGroup_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: GroupViewModel vm })
        {
            _draggedGroup = FindAppGroup(vm.Id);
            _dragGroupStartPoint = e.GetPosition(this);
        }
        else
        {
            _draggedGroup = null;
        }
    }

    private void DockGroup_PreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e) => _draggedGroup = null;

    private void DockGroup_PreviewMouseMove(object sender, MouseEventArgs e)
    {
        if (e.LeftButton != MouseButtonState.Pressed || _draggedGroup is null || sender is not FrameworkElement element)
            return;
        var delta = e.GetPosition(this) - _dragGroupStartPoint;
        if (Math.Abs(delta.X) < SystemParameters.MinimumHorizontalDragDistance
            && Math.Abs(delta.Y) < SystemParameters.MinimumVerticalDragDistance)
        {
            return;
        }
        var group = _draggedGroup;
        _draggedGroup = null;
        _foregroundAtAppPress = null;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        ResetMagnification(immediate: true);
        try
        {
            DragDrop.DoDragDrop(element, group, DragDropEffects.Move);
        }
        finally
        {
            CancelLiveDrag();
            ResetDisplacements();
        }
    }

    private void DockGroup_DragOver(object sender, DragEventArgs e)
    {
        if (e.Data.GetData(typeof(DockApp)) is DockApp app && sender is FrameworkElement { DataContext: GroupViewModel })
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }
        if (TryGetDockItemToken(e, out _))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }
        e.Effects = DragDropEffects.None;
        e.Handled = true;
    }

    private void DockGroup_DragLeave(object sender, DragEventArgs e) { }

    private void DockGroup_Drop(object sender, DragEventArgs e)
    {
        if (sender is not FrameworkElement { DataContext: GroupViewModel vm }) return;
        var group = FindAppGroup(vm.Id);
        if (group is null) return;

        if (e.Data.GetData(typeof(DockApp)) is DockApp app)
        {
            CancelLiveDrag();
            // Dropping an app onto a group icon adds it (removing it from any previous group).
            RemoveAppFromItsGroup(app, save: false, preserveGroupId: group.Id);
            var groups = AppGroups.Select(g => SamePath(g.Id, group.Id)
                ? g with { ExecutablePaths = g.ExecutablePaths.Append(app.DockKey).Distinct(StringComparer.OrdinalIgnoreCase).ToList() }
                : g).ToList();
            SaveGroups(groups);
            e.Handled = true;
            return;
        }
    }
}
