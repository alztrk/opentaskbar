//! Country/city catalogue and weather helpers (C# `CountryCityService.cs`, `CountryFlag.cs`,
//! `WeatherService.cs` parity).
//!
//! - `countries()` / `cities(country)` — countriesnow.space API with an offline fallback of
//!   21 countries and all 81 Turkish provinces.
//! - `find_cached_country_for_city()` — reverse lookup over the offline catalogue.
//! - `fetch_cities()` — live API query with an on-disk cache under `%LOCALAPPDATA%\OpenTaskbar`.
//! - Weather helpers: language normalisation, `dayCount` clamping (C# keeps 1..7),
//!   `describe()` icon keys for the forecast rows.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};

/// `(ISO code, display name)` for the 21 C# fallback countries.
pub const COUNTRIES: &[(&str, &str)] = &[
    ("TR", "Turkey"),
    ("US", "United States"),
    ("GB", "United Kingdom"),
    ("DE", "Germany"),
    ("FR", "France"),
    ("ES", "Spain"),
    ("IT", "Italy"),
    ("NL", "Netherlands"),
    ("PL", "Poland"),
    ("RU", "Russia"),
    ("UA", "Ukraine"),
    ("JP", "Japan"),
    ("KR", "South Korea"),
    ("CN", "China"),
    ("BR", "Brazil"),
    ("CA", "Canada"),
    ("AU", "Australia"),
    ("IN", "India"),
    ("MX", "Mexico"),
    ("SE", "Sweden"),
    ("GR", "Greece"),
];

/// All 81 Turkish provinces (C# `CountryCityService.TurkeyFallback`).
pub const TURKEY_CITIES: &[&str] = &[
    "Adana",
    "Adıyaman",
    "Afyonkarahisar",
    "Ağrı",
    "Amasya",
    "Ankara",
    "Antalya",
    "Ardahan",
    "Artvin",
    "Aydın",
    "Balıkesir",
    "Bartın",
    "Batman",
    "Bayburt",
    "Bilecik",
    "Bingöl",
    "Bitlis",
    "Bolu",
    "Burdur",
    "Bursa",
    "Çanakkale",
    "Çankırı",
    "Çorum",
    "Denizli",
    "Diyarbakır",
    "Düzce",
    "Edirne",
    "Elazığ",
    "Erzincan",
    "Erzurum",
    "Eskişehir",
    "Gaziantep",
    "Giresun",
    "Gümüşhane",
    "Hakkâri",
    "Hatay",
    "Iğdır",
    "Isparta",
    "İstanbul",
    "İzmir",
    "Kars",
    "Kastamonu",
    "Kayseri",
    "Kırklareli",
    "Kırıkkale",
    "Kırşehir",
    "Kilis",
    "Kocaeli",
    "Konya",
    "Kütahya",
    "Malatya",
    "Manisa",
    "Kahramanmaraş",
    "Mardin",
    "Mersin",
    "Muğla",
    "Muş",
    "Nevşehir",
    "Niğde",
    "Ordu",
    "Osmaniye",
    "Rize",
    "Sakarya",
    "Samsun",
    "Şanlıurfa",
    "Siirt",
    "Sinop",
    "Sivas",
    "Şırnak",
    "Tekirdağ",
    "Tokat",
    "Trabzon",
    "Tunceli",
    "Uşak",
    "Van",
    "Yalova",
    "Yozgat",
    "Zonguldak",
    "Aksaray",
    "Karaman",
    "Karabük",
];

/// Offline fallback for other countries (small, deterministic).
const GENERIC_CITIES: &[&str] = &[
    "Capital City",
    "North District",
    "South District",
    "East District",
    "West District",
    "Central",
    "Coastal",
    "Highland",
];

static CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("OpenTaskbar")
}

fn cache_file() -> PathBuf {
    cache_dir().join("country-city-cache.json")
}

/// 21 countries with stable ISO codes and display names.
pub fn countries() -> Vec<(String, String)> {
    COUNTRIES
        .iter()
        .map(|(c, n)| ((*c).to_owned(), (*n).to_owned()))
        .collect()
}

fn is_turkey(country: &str) -> bool {
    let c = country.trim().to_ascii_lowercase();
    matches!(c.as_str(), "tr" | "tur" | "turkey" | "türkiye" | "turkiye")
}

fn code_for(country: &str) -> Option<&'static str> {
    let c = country.trim().to_ascii_lowercase();
    COUNTRIES
        .iter()
        .find(|(code, name)| {
            code.eq_ignore_ascii_case(&c)
                || name.to_ascii_lowercase() == c
                || is_turkey(name) && is_turkey(country)
        })
        .map(|(code, _)| *code)
}

/// Cities for a country. Turkey always returns all 81 provinces; other countries use the
/// countriesnow.space cache (when populated) and a small deterministic fallback.
pub fn cities(country: &str) -> Vec<String> {
    let code = code_for(country).unwrap_or_else(|| country.trim());
    if is_turkey(country) || code.eq_ignore_ascii_case("TR") {
        return TURKEY_CITIES.iter().map(|s| (*s).to_owned()).collect();
    }
    if let Ok(map) = CACHE.lock()
        && let Some(list) = map.get(&code.to_ascii_uppercase())
    {
        return list.clone();
    }
    GENERIC_CITIES.iter().map(|s| (*s).to_owned()).collect()
}

/// Reverse lookup used by the settings city picker (C# `FindCachedCountryForCity`).
pub fn find_cached_country_for_city(city: &str) -> Option<&'static str> {
    let needle = city.trim().to_ascii_lowercase();
    if TURKEY_CITIES
        .iter()
        .any(|c| c.to_ascii_lowercase() == needle)
    {
        return Some("TR");
    }
    if let Ok(map) = CACHE.lock() {
        for (code, list) in map.iter() {
            if list.iter().any(|c| c.to_ascii_lowercase() == needle) {
                return COUNTRIES.iter().find(|(c, _)| *c == code).map(|(c, _)| *c);
            }
        }
    }
    None
}

/// Pure parser for countriesnow.space `/countries/cities` payloads.
pub fn parse_cities_payload(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let value = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.as_array());
    let Some(items) = value else {
        return Vec::new();
    };
    let mut out: Vec<String> = items
        .iter()
        .filter_map(|x| {
            let s = x.as_str()?;
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_owned())
        })
        .collect();
    out.sort_by_key(|s| s.to_ascii_lowercase());
    out.dedup_by_key(|s| s.to_ascii_lowercase());
    out
}

fn winhttp_get(host: &str, path: &str) -> Option<String> {
    let agent: Vec<u16> = "OpenTaskbar/1.0\0".encode_utf16().collect();
    let host_w: Vec<u16> = host.encode_utf16().chain(Some(0)).collect();
    let path_w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let verb_w: Vec<u16> = "GET\0".encode_utf16().collect();
    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return None;
        }
        let _ = WinHttpSetTimeouts(session, 2000, 2000, 3000, 4000);
        let connect = WinHttpConnect(session, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            WinHttpCloseHandle(session);
            return None;
        }
        let req = WinHttpOpenRequest(
            connect,
            verb_w.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            WinHttpCloseHandle(connect);
            WinHttpCloseHandle(session);
            return None;
        }
        let sent = WinHttpSendRequest(req, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) != 0
            && WinHttpReceiveResponse(req, std::ptr::null_mut()) != 0;
        let mut body = Vec::new();
        if sent {
            loop {
                let mut avail: u32 = 0;
                if windows_sys::Win32::Networking::WinHttp::WinHttpQueryDataAvailable(
                    req, &mut avail,
                ) == 0
                    || avail == 0
                {
                    break;
                }
                if body.len() + avail as usize > 256 * 1024 {
                    break;
                }
                let start = body.len();
                body.resize(start + avail as usize, 0);
                let mut read = 0u32;
                if WinHttpReadData(req, body[start..].as_mut_ptr().cast(), avail, &mut read) == 0
                    || read == 0
                {
                    body.truncate(start);
                    break;
                }
                body.truncate(start + read as usize);
            }
        }
        WinHttpCloseHandle(req);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        sent.then(|| String::from_utf8_lossy(&body).into_owned())
    }
}

/// Live countriesnow.space lookup, cached in memory and on disk.
pub fn fetch_cities(country: &str) -> Vec<String> {
    let code = code_for(country).unwrap_or_else(|| country.trim());
    let key = code.to_ascii_uppercase();
    if let Ok(map) = CACHE.lock()
        && let Some(list) = map.get(&key)
    {
        return list.clone();
    }
    let path = format!("/api/v0.1/countries/cities?q={}", urlencode(country));
    let list = winhttp_get("countriesnow.space", &path)
        .map(|body| parse_cities_payload(&body))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| cities(country));
    if let Ok(mut map) = CACHE.lock() {
        map.insert(key.clone(), list.clone());
    }
    let path = cache_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let mut merged: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = merged.as_object_mut() {
            obj.insert(key, serde_json::to_value(&list).unwrap_or_default());
        }
        let _ = std::fs::write(&path, serde_json::to_string(&merged).unwrap_or_default());
    } else {
        let mut root = serde_json::Map::new();
        root.insert(key, serde_json::to_value(&list).unwrap_or_default());
        let _ = std::fs::write(&path, serde_json::to_string(&root).unwrap_or_default());
    }
    list
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Outcome of the Windows location-capability check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocationAccess {
    Granted,
    Denied,
    Unavailable,
}

/// A single raw geolocation fix emitted by the WinRT probe (status + optional address/coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct GeolocationSample {
    /// Raw `GeolocationAccessStatus` value (1 = Allowed, 2/3/4 = denied/disabled).
    pub access: i32,
    pub city: String,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl GeolocationSample {
    pub fn has_coordinates(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && self.latitude.abs() <= 90.0
            && self.longitude.abs() <= 180.0
            && (self.latitude != 0.0 || self.longitude != 0.0)
    }
}

/// Maps `Windows.Devices.Geolocation.GeolocationAccessStatus` to the decision-table enum.
pub fn location_access_from_status(status: i32) -> LocationAccess {
    match status {
        1 => LocationAccess::Granted,
        2..=4 => LocationAccess::Denied,
        _ => LocationAccess::Unavailable,
    }
}

/// Pure auto-fill decision: only an empty saved city with granted access may be overwritten.
pub fn should_auto_geolocate(saved_city: &str, access: LocationAccess) -> bool {
    saved_city.trim().is_empty() && access == LocationAccess::Granted
}

/// Parses the `status|city|country|lat|lon` line printed by the PowerShell probe.
pub fn parse_geolocation_response(raw: &str) -> Option<GeolocationSample> {
    let line = raw.lines().find(|line| !line.trim().is_empty())?.trim();
    let mut fields = line.split('|');
    let access = fields.next()?.trim().parse::<i32>().ok()?;
    let city = fields.next().unwrap_or_default().trim().to_owned();
    let country = fields.next().unwrap_or_default().trim().to_owned();
    let latitude = fields
        .next()
        .unwrap_or_default()
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0);
    let longitude = fields
        .next()
        .unwrap_or_default()
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0);
    Some(GeolocationSample {
        access,
        city,
        country,
        latitude,
        longitude,
    })
}

/// Converts a country hint (ISO code, English or Turkish name) to the catalogue display name.
/// Unknown or empty hints fall back to the offline catalogue's first country.
pub fn country_display_name(hint: &str) -> String {
    let hint = hint.trim();
    if hint.is_empty() {
        return COUNTRIES[0].1.to_owned();
    }
    if let Some((_, name)) = COUNTRIES.iter().find(|(code, name)| {
        code.eq_ignore_ascii_case(hint)
            || name.eq_ignore_ascii_case(hint)
            || (is_turkey(name) && is_turkey(hint))
    }) {
        return (*name).to_owned();
    }
    hint.to_owned()
}

/// Resolves a city/country hint pair through the offline catalogue. A missing country hint is
/// inferred from the cached city list before falling back to the catalogue default.
pub fn resolve_location(country_hint: &str, city_hint: &str) -> Option<(String, String)> {
    let city = city_hint.trim();
    if city.is_empty() {
        return None;
    }
    let country = if country_hint.trim().is_empty() {
        find_cached_country_for_city(city)
            .map(country_display_name)
            .unwrap_or_else(|| country_display_name(""))
    } else {
        country_display_name(country_hint)
    };
    Some((country, city.to_owned()))
}

/// Reads the per-user Windows "Let apps access your location" consent store. `None` means the
/// registry is inconclusive, in which case `RequestAccessAsync` remains authoritative.
fn registry_location_permission() -> Option<bool> {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    let path: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\location\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "Value\0".encode_utf16().collect();
    unsafe {
        let mut key: *mut std::ffi::c_void = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return None;
        }
        let mut kind = 0u32;
        let mut data = [0u16; 32];
        let mut len = (data.len() * 2) as u32;
        let ok = RegQueryValueExW(
            key,
            value.as_ptr(),
            std::ptr::null(),
            &mut kind,
            data.as_mut_ptr().cast(),
            &mut len,
        ) == 0;
        RegCloseKey(key);
        if !ok || kind != REG_SZ {
            return None;
        }
        let count = (len as usize / 2).min(data.len());
        let consent = String::from_utf16_lossy(&data[..count])
            .trim()
            .to_ascii_lowercase();
        match consent.as_str() {
            "allow" => Some(true),
            "deny" => Some(false),
            _ => None,
        }
    }
}

fn geolocation_sample() -> Option<GeolocationSample> {
    let ps = r#"
$ErrorActionPreference = 'Stop'
try {
  Add-Type -AssemblyName System.Runtime.WindowsRuntime
  $t = [Windows.Devices.Geolocation.Geolocator, Windows.Devices.Geolocation, ContentType = WindowsRuntime]
  $status = [int]($t::RequestAccessAsync().AsTask().GetAwaiter().GetResult())
  if ($status -ne 1) { "$status|||0|0"; exit }
  $g = New-Object $t
  $g.DesiredAccuracy = 100
  $p = $g.GetGeopositionAsync().AsTask().GetAwaiter().GetResult()
  $city = $p.CivicAddress.City
  $country = $p.CivicAddress.Country
  $lat = $p.Coordinate.Point.Position.Latitude
  $lon = $p.Coordinate.Point.Position.Longitude
  "{0}|{1}|{2}|{3}|{4}" -f $status, $city, $country, $lat, $lon
} catch {
  "-1|||0|0"
}
"#;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    parse_geolocation_response(&String::from_utf8_lossy(&out.stdout))
}

/// Best-effort Windows geolocation: checks the capability consent store, asks WinRT for
/// permission, then resolves a `(country, city)` pair from the civic address, reverse geocoding
/// of the coordinate fix, or the offline catalogue. `None` means denied/unavailable — callers
/// silently keep their saved location.
pub fn geolocate() -> Option<(String, String)> {
    if registry_location_permission() == Some(false) {
        return None;
    }
    let sample = geolocation_sample()?;
    if location_access_from_status(sample.access) != LocationAccess::Granted {
        return None;
    }
    if let Some(pair) = resolve_location(&sample.country, &sample.city) {
        return Some(pair);
    }
    if sample.has_coordinates()
        && let Some(geo) =
            crate::widgets::telemetry::reverse_geocode(sample.latitude, sample.longitude, "en")
    {
        return resolve_location(&geo.country, &geo.name);
    }
    None
}

/// Windows Geolocation (best-effort). Returns a display city name, or `None` when denied.
pub fn geolocate_city() -> Option<String> {
    geolocate().map(|(_, city)| city)
}

/// Normalises the UI language to an Open-Meteo `language` query value.
pub fn weather_language(lang: &str) -> &'static str {
    let l = lang.trim().to_ascii_lowercase();
    if l.starts_with("tr") {
        "tr"
    } else if l.starts_with("de") {
        "de"
    } else if l.starts_with("fr") {
        "fr"
    } else if l.starts_with("es") {
        "es"
    } else if l.starts_with("it") {
        "it"
    } else if l.starts_with("ja") {
        "ja"
    } else if l.starts_with("ko") {
        "ko"
    } else if l.starts_with("ru") {
        "ru"
    } else if l.starts_with("pl") {
        "pl"
    } else if l.starts_with("nl") {
        "nl"
    } else if l.starts_with("zh") {
        "zh"
    } else if l.starts_with("uk") {
        "uk"
    } else if l.starts_with("pt") {
        "pt"
    } else {
        "en"
    }
}

/// C# `WeatherService` protects `dayCount` by clamping to 1..=7.
pub fn clamp_day_count(n: i32) -> u8 {
    n.clamp(1, 7) as u8
}

/// `Describe()` icon key for a WMO weather code (used by the forecast icon glyph switch).
pub fn weather_icon_key(code: i64) -> &'static str {
    match code {
        0 => "Clear",
        1 | 2 => "PartlyCloudy",
        3 => "Cloudy",
        45 | 48 => "Fog",
        51..=57 => "Drizzle",
        61..=67 => "Rain",
        71..=77 => "Snow",
        80..=82 => "Showers",
        85 | 86 => "SnowShowers",
        95..=99 => "Thunderstorm",
        _ => "Cloudy",
    }
}

/// Builds the Open-Meteo forecast path with a language and sanitised day count.
pub fn forecast_path(lat: f64, lon: f64, language: &str, day_count: i32) -> String {
    format!(
        "/v1/forecast?latitude={lat:.3}&longitude={lon:.3}&current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m&daily=weather_code,temperature_2m_max,temperature_2m_min&forecast_days={}&timezone=auto&language={}",
        clamp_day_count(day_count),
        weather_language(language)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_has_21_countries_and_81_provinces() {
        assert_eq!(COUNTRIES.len(), 21);
        assert_eq!(TURKEY_CITIES.len(), 81);
        assert_eq!(cities("TR").len(), 81);
        assert_eq!(cities("turkiye").len(), 81);
        assert!(cities("DE").len() >= 8);
    }

    #[test]
    fn reverse_city_lookup() {
        assert_eq!(find_cached_country_for_city("İstanbul"), Some("TR"));
        assert_eq!(find_cached_country_for_city("  ankara  "), Some("TR"));
        assert_eq!(find_cached_country_for_city("Atlantis"), None);
    }

    #[test]
    fn countriesnow_payload_parser() {
        let payload = r#"{"data":["Izmir","Ankara","Izmir","","Antalya"]}"#;
        let parsed = parse_cities_payload(payload);
        assert_eq!(parsed, vec!["Ankara", "Antalya", "Izmir"]);
        assert!(parse_cities_payload("not json").is_empty());
    }

    #[test]
    fn geolocation_decision_table() {
        assert_eq!(location_access_from_status(1), LocationAccess::Granted);
        for denied in [2, 3, 4] {
            assert_eq!(location_access_from_status(denied), LocationAccess::Denied);
        }
        for unknown in [0, -1, 99] {
            assert_eq!(
                location_access_from_status(unknown),
                LocationAccess::Unavailable
            );
        }
        // Only an empty saved city with granted access is overwritten.
        assert!(should_auto_geolocate("", LocationAccess::Granted));
        assert!(should_auto_geolocate("  ", LocationAccess::Granted));
        assert!(!should_auto_geolocate(
            "Izmir, Turkey",
            LocationAccess::Granted
        ));
        assert!(!should_auto_geolocate("", LocationAccess::Denied));
        assert!(!should_auto_geolocate("", LocationAccess::Unavailable));
    }

    #[test]
    fn geolocation_response_and_catalog_resolution() {
        let sample = parse_geolocation_response("1|İstanbul|Turkey|41.008|28.978\n")
            .expect("probe line must parse");
        assert_eq!(sample.access, 1);
        assert_eq!(sample.city, "İstanbul");
        assert!(sample.has_coordinates());
        assert_eq!(
            resolve_location(&sample.country, &sample.city),
            Some(("Turkey".to_owned(), "İstanbul".to_owned()))
        );
        // Denied status still parses, but the decision table blocks the auto-fill.
        let denied = parse_geolocation_response("2|||0|0").unwrap();
        assert_eq!(
            location_access_from_status(denied.access),
            LocationAccess::Denied
        );
        assert!(!denied.has_coordinates());
        assert!(parse_geolocation_response("nonsense").is_none());
        // Empty country hints are recovered from the offline city catalogue.
        assert_eq!(
            resolve_location("", "Ankara"),
            Some(("Turkey".to_owned(), "Ankara".to_owned()))
        );
        assert_eq!(country_display_name("TR"), "Turkey");
        assert_eq!(country_display_name("Türkiye"), "Turkey");
        assert_eq!(resolve_location("TR", ""), None);
    }

    #[test]
    fn weather_helpers() {
        assert_eq!(weather_language("tr-TR"), "tr");
        assert_eq!(weather_language("xx-XX"), "en");
        assert_eq!(clamp_day_count(0), 1);
        assert_eq!(clamp_day_count(99), 7);
        assert_eq!(weather_icon_key(95), "Thunderstorm");
        assert_eq!(weather_icon_key(3), "Cloudy");
        let path = forecast_path(41.0, 29.0, "tr", 3);
        assert!(path.contains("forecast_days=3"));
        assert!(path.contains("language=tr"));
    }
}
