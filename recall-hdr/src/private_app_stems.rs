//! The exe stems behind `private_apps` (data only; the rules are there).

/// Stems checked by `record_target::is_target` and `rules::classify`.
/// "(seen)" = the exe name was read on this PC (install folder or the
/// package's AppxManifest, 2026-11-07); the others are the vendors' known
/// install names, not verified here.
pub(crate) const PRIVATE_APP_STEMS: &[&str] = &[
    // Meetings and calls.
    "zoom",     // (seen) C:\Program Files\Zoom\bin\Zoom.exe
    "cpthost",  // (seen) Zoom's screen-share host, same folder
    "ms-teams", // (seen) new Teams, MSTeams package
    "msteams",  // new Teams' earlier name
    "teams",    // classic Teams
    "webex",
    "ciscocollabhost", // Webex app
    "atmgr",           // Webex Meetings (classic)
    "skype",
    "lync", // Skype for Business
    // Chat.
    "slack",
    "whatsapp",
    "whatsapp.root", // (seen) WhatsApp Desktop package, WhatsApp.Root.exe
    "telegram",
    "signal",
    "viber",
    "phoneexperiencehost", // (seen) Phone Link, Microsoft.YourPhone package
    "messenger",           // Facebook Messenger desktop
    "wechat",              // WeChat 3.x
    "weixin",              // WeChat 4.x
    "line",
    "element", // Matrix client
    "kakaotalk",
    "mattermost",
    "rocket.chat",
    "wire",
    "zulip",
    // Mail.
    "outlook",
    "olk", // (seen) new Outlook, Microsoft.OutlookForWindows package
    "thunderbird",
    // Office, slideshows, documents, notes.
    "powerpnt",
    "winword",
    "excel",
    "onenote",
    "acrobat",
    "acrord32",
    "notion",
    "obsidian", // (seen) C:\Program Files\Obsidian\Obsidian.exe
    "figma",
    "soffice", // (seen) LibreOffice: the window belongs to soffice.bin (stem soffice)
    "simpress", // (seen) LibreOffice launchers, C:\Program Files\LibreOffice\program
    "swriter",  // (seen)
    "scalc",    // (seen)
    "sdraw",    // (seen)
    "smath",    // (seen)
    "sbase",    // (seen)
    "sweb",     // (seen)
    "wps",      // WPS Office Writer; its spreadsheet `et` is left out (Enemy Territory ships ET.exe)
    "wpp",      // WPS Office Presentation
    "desktopeditors", // ONLYOFFICE Desktop Editors
    // Media players and video apps (`record_target::TARGET_DENY_STEMS` also
    // denies the classic players; listed here too so the HDR rules leave them
    // NonGame). Kodi renders like a game (D3D11, can get independent flip),
    // so the render check alone would record it.
    "kodi",
    "plex",
    "plex htpc",
    "stremio",          // Stremio 4.x
    "stremio-shell-ng", // (seen) C:\Users\rohan\AppData\Local\Programs\Stremio, mpv inside
    "jellyfinmediaplayer",
    "vlc",
    "mpv",
    "mpvnet",
    "mpc-hc",
    "mpc-hc64",
    "mpc-be",
    "mpc-be64",
    "potplayer",
    "potplayer64",
    "potplayermini",
    "potplayermini64",
    "smplayer",
    "wmplayer",
    "primevideo", // Prime Video for Windows (Netflix is in `rules::STORE_APP_STEMS`)
    "disneyplus",
    // Music and other Electron-type apps.
    "spotify",
    "foobar2000",                // (seen) C:\Program Files\foobar2000
    "tidal",                     // (seen) %LOCALAPPDATA%\TIDAL
    "youtube music desktop app", // (seen) %LOCALAPPDATA%\Programs\youtube-music-desktop-app
    "itunes",
    "applemusic",
    "electron", // a stock Electron runtime; a Steam-library exe of this name still records
];
