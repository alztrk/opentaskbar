$ErrorActionPreference = 'Stop'
$dir = 'D:\projects\OpenTaskbar\Services\Localization\Languages'
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

function J($s) { '"' + ($s -replace '\\','\\' -replace '"','\"' -replace "`r",'\r' -replace "`n",'\n') + '"' }

$en = @{
    ThemeFieldTextPrimary='Primary text'; ThemeFieldTextSecondary='Secondary text'
    ThemeFieldAccentBrush='Accent color'; ThemeFieldAccentText='Accent text color'
    ThemeFieldStrokeSubtle='Border color'; ThemeFieldStrokeFocus='Focus ring color'
    ThemeFieldSurfaceSelected='Selected surface'; ThemeFieldSurfaceHover='Hover surface'
    ThemeFieldDockBackgroundMode='Dock material'; ThemeFieldDockMaterial='Dock background'
    ThemeFieldDockBorderMaterial='Dock border'; ThemeFieldDockHighlight='Dock highlight'
    ThemeFieldDockRadius='Dock corner radius'; ThemeFieldDockInnerRadius='Dock inner radius'
    ThemeFieldDockWidgetPadding='Widget padding'; ThemeFieldDockShadowBlur='Shadow blur'
    ThemeFieldDockShadowOpacity='Shadow opacity'; ThemeFieldDockShadowColor='Shadow color'
    ThemeFieldWidgetCard='Widget surface'; ThemeFieldWidgetCardStroke='Widget border'
    ThemeFieldWidgetTrack='Widget meter track'; ThemeFieldWidgetAccent='Widget meter fill'
    ThemeFieldIconForeground='Utility icon color'; ThemeFieldWindowsLogoBrush='Start logo color'
    ThemeFieldIconHoverGlow='Icon hover glow'; ThemeFieldStartTileBackground='Start tile background'
    ThemeFieldStartTileStroke='Start tile border'; ThemeFieldStartTileHover='Start tile hover'
    ThemeFieldWidgetWarning='Widget warning color'; ThemeFieldWidgetFocusAccent='Widget focus color'
    ThemeFieldDockWidgetSurface='Dock widget background'; ThemeFieldDockWidgetRadius='Dock widget radius'
    ThemeFieldWidgetSpacing='Widget spacing'; ThemeFieldWidgetBackground='Widget background'
    ThemeFieldSettingsBackground='Settings window background'; ThemeFieldSettingsBackgroundOpacity='Settings window opacity'
    ThemeFieldSettingsControl='Control surface'; ThemeFieldSettingsControlHover='Control hover surface'
    ThemeFieldSettingsSidebarOverlay='Sidebar surface'; ThemeFieldMenuMaterial='Menu background'
    ThemeFieldMenuDivider='Menu divider'; ThemeFieldSurfacePopup='Popup surface'
    ThemeFieldMenuLabelWidth='Menu label width'; ThemeFieldPopupRadius='Popup corner radius'
    ThemeFieldPopupInnerRadius='Popup inner radius'; ThemeFieldInterfaceFont='Font family'
    ThemeFieldTextSizeBody='Body font size'; ThemeFieldTextSizeCaption='Caption font size'
    ThemeFieldControlRadius='Control corner radius'; ThemeFieldSettingsPageInset='Page padding'
    ThemeFieldName='Name'; ThemeFieldAuthor='Author'; ThemeFieldDescription='Description'
    ThemeWidgetPackSuffix='Widgets'; ThemeNewEditableName='My {0} Theme'
    ThemeFontFieldHelp='Font family name. Embedded fonts are resolved automatically.'
    ThemeFourValueFieldHelp='One value for all sides, or four values separated by commas.'
}

$tr = @{
    ThemeFieldTextPrimary='Birincil metin'; ThemeFieldTextSecondary='İkincil metin'
    ThemeFieldAccentBrush='Vurgu rengi'; ThemeFieldAccentText='Vurgu metin rengi'
    ThemeFieldStrokeSubtle='Kenarlık rengi'; ThemeFieldStrokeFocus='Odak halkası rengi'
    ThemeFieldSurfaceSelected='Seçili yüzey'; ThemeFieldSurfaceHover='Üzerine gelinen yüzey'
    ThemeFieldDockBackgroundMode='Dock malzemesi'; ThemeFieldDockMaterial='Dock arka planı'
    ThemeFieldDockBorderMaterial='Dock kenarlığı'; ThemeFieldDockHighlight='Dock vurgusu'
    ThemeFieldDockRadius='Dock köşe yarıçapı'; ThemeFieldDockInnerRadius='Dock iç yarıçapı'
    ThemeFieldDockWidgetPadding='Widget dolgusu'; ThemeFieldDockShadowBlur='Gölge bulanıklığı'
    ThemeFieldDockShadowOpacity='Gölge opaklığı'; ThemeFieldDockShadowColor='Gölge rengi'
    ThemeFieldWidgetCard='Widget yüzeyi'; ThemeFieldWidgetCardStroke='Widget kenarlığı'
    ThemeFieldWidgetTrack='Widget ölçer çizgisi'; ThemeFieldWidgetAccent='Widget ölçer dolgusu'
    ThemeFieldIconForeground='Yardımcı simge rengi'; ThemeFieldWindowsLogoBrush='Başlat logosu rengi'
    ThemeFieldIconHoverGlow='Simge üzerine gelme parlaması'; ThemeFieldStartTileBackground='Başlat kutusu arka planı'
    ThemeFieldStartTileStroke='Başlat kutusu kenarlığı'; ThemeFieldStartTileHover='Başlat kutusu üzerine gelme'
    ThemeFieldWidgetWarning='Widget uyarı rengi'; ThemeFieldWidgetFocusAccent='Widget odak rengi'
    ThemeFieldDockWidgetSurface='Dock widget arka planı'; ThemeFieldDockWidgetRadius='Dock widget yarıçapı'
    ThemeFieldWidgetSpacing='Widget aralığı'; ThemeFieldWidgetBackground='Widget arka planı'
    ThemeFieldSettingsBackground='Ayarlar penceresi arka planı'; ThemeFieldSettingsBackgroundOpacity='Ayarlar penceresi opaklığı'
    ThemeFieldSettingsControl='Denetim yüzeyi'; ThemeFieldSettingsControlHover='Denetim üzerine gelme yüzeyi'
    ThemeFieldSettingsSidebarOverlay='Kenar çubuğu yüzeyi'; ThemeFieldMenuMaterial='Menü arka planı'
    ThemeFieldMenuDivider='Menü ayırıcı'; ThemeFieldSurfacePopup='Açılır pencere yüzeyi'
    ThemeFieldMenuLabelWidth='Menü etiket genişliği'; ThemeFieldPopupRadius='Açılır pencere köşe yarıçapı'
    ThemeFieldPopupInnerRadius='Açılır pencere iç yarıçapı'; ThemeFieldInterfaceFont='Yazı tipi ailesi'
    ThemeFieldTextSizeBody='Gövde yazı boyutu'; ThemeFieldTextSizeCaption='Başlık yazı boyutu'
    ThemeFieldControlRadius='Denetim köşe yarıçapı'; ThemeFieldSettingsPageInset='Sayfa dolgusu'
    ThemeFieldName='Ad'; ThemeFieldAuthor='Yazar'; ThemeFieldDescription='Açıklama'
    ThemeWidgetPackSuffix='Widgetlar'; ThemeNewEditableName='{0} Temam'
    ThemeFontFieldHelp='Yazı tipi ailesi adı. Gömülü yazı tipleri otomatik çözümlenir.'
    ThemeFourValueFieldHelp='Tüm kenarlar için tek değer veya virgülle ayrılmış dört değer.'
}

$de = @{
    ThemeFieldTextPrimary='Primärer Text'; ThemeFieldTextSecondary='Sekundärer Text'
    ThemeFieldAccentBrush='Akzentfarbe'; ThemeFieldAccentText='Akzent-Textfarbe'
    ThemeFieldStrokeSubtle='Rahmenfarbe'; ThemeFieldStrokeFocus='Fokusringfarbe'
    ThemeFieldSurfaceSelected='Ausgewählte Oberfläche'; ThemeFieldSurfaceHover='Hover-Oberfläche'
    ThemeFieldDockBackgroundMode='Dock-Material'; ThemeFieldDockMaterial='Dock-Hintergrund'
    ThemeFieldDockBorderMaterial='Dock-Rahmen'; ThemeFieldDockHighlight='Dock-Hervorhebung'
    ThemeFieldDockRadius='Dock-Eckenradius'; ThemeFieldDockInnerRadius='Dock-Innenradius'
    ThemeFieldDockWidgetPadding='Widget-Abstand'; ThemeFieldDockShadowBlur='Schattenunschärfe'
    ThemeFieldDockShadowOpacity='Schattendeckkraft'; ThemeFieldDockShadowColor='Schattenfarbe'
    ThemeFieldWidgetCard='Widget-Oberfläche'; ThemeFieldWidgetCardStroke='Widget-Rahmen'
    ThemeFieldWidgetTrack='Widget-Messerleiste'; ThemeFieldWidgetAccent='Widget-Messerfüllung'
    ThemeFieldIconForeground='Symbolfarbe'; ThemeFieldWindowsLogoBrush='Start-Logo-Farbe'
    ThemeFieldIconHoverGlow='Symbol-Hover-Glanz'; ThemeFieldStartTileBackground='Start-Kachel-Hintergrund'
    ThemeFieldStartTileStroke='Start-Kachel-Rahmen'; ThemeFieldStartTileHover='Start-Kachel-Hover'
    ThemeFieldWidgetWarning='Widget-Warnfarbe'; ThemeFieldWidgetFocusAccent='Widget-Fokusfarbe'
    ThemeFieldDockWidgetSurface='Dock-Widget-Hintergrund'; ThemeFieldDockWidgetRadius='Dock-Widget-Radius'
    ThemeFieldWidgetSpacing='Widget-Abstand'; ThemeFieldWidgetBackground='Widget-Hintergrund'
    ThemeFieldSettingsBackground='Einstellungsfenster-Hintergrund'; ThemeFieldSettingsBackgroundOpacity='Einstellungsfenster-Deckkraft'
    ThemeFieldSettingsControl='Steuerelement-Oberfläche'; ThemeFieldSettingsControlHover='Steuerelement-Hover-Oberfläche'
    ThemeFieldSettingsSidebarOverlay='Seitenleisten-Oberfläche'; ThemeFieldMenuMaterial='Menü-Hintergrund'
    ThemeFieldMenuDivider='Menü-Trenner'; ThemeFieldSurfacePopup='Popup-Oberfläche'
    ThemeFieldMenuLabelWidth='Menü-Labelbreite'; ThemeFieldPopupRadius='Popup-Eckenradius'
    ThemeFieldPopupInnerRadius='Popup-Innenradius'; ThemeFieldInterfaceFont='Schriftfamilie'
    ThemeFieldTextSizeBody='Textgröße'; ThemeFieldTextSizeCaption='Beschriftungsgröße'
    ThemeFieldControlRadius='Steuerelement-Eckenradius'; ThemeFieldSettingsPageInset='Seitenabstand'
    ThemeFieldName='Name'; ThemeFieldAuthor='Autor'; ThemeFieldDescription='Beschreibung'
    ThemeWidgetPackSuffix='Widgets'; ThemeNewEditableName='Mein {0}-Design'
    ThemeFontFieldHelp='Name der Schriftfamilie. Eingebettete Schriften werden automatisch aufgelöst.'
    ThemeFourValueFieldHelp='Ein Wert für alle Seiten oder vier durch Kommas getrennte Werte.'
}

$overrides = @{ 'tr-TR' = $tr; 'de-DE' = $de }

foreach ($file in Get-ChildItem $dir -Filter *.json) {
    $culture = $file.BaseName
    # Read as UTF-8 explicitly; never let PowerShell's ANSI default reinterpret the bytes.
    $content = [System.IO.File]::ReadAllText($file.FullName, [System.Text.Encoding]::UTF8)
    $table = @{}
    foreach ($p in $en.GetEnumerator()) { $table[$p.Key] = $p.Value }
    if ($overrides.ContainsKey($culture)) { foreach ($p in $overrides[$culture].GetEnumerator()) { $table[$p.Key] = $p.Value } }

    foreach ($k in $en.Keys) {
        $pattern = '(?m)^(\s*)"' + [regex]::Escape($k) + '"\s*:\s*("(?:\\.|[^"\\])*")(\s*,?)\s*$'
        $replacement = '$1"' + $k + '": ' + (J $table[$k]) + '$3'
        $content = [regex]::Replace($content, $pattern, $replacement)
    }
    [System.IO.File]::WriteAllText($file.FullName, $content, $utf8NoBom)
    Write-Output ("Fixed " + $culture)
}
