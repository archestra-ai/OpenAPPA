# Development copy. `appa init claude-code` overwrites this file in the
# deployment it materializes, with the absolute paths that init resolved and no
# environment fallback at all.
#
# This copy exists so a checkout stays usable with --plugin-dir, where nothing
# has been rendered yet.
$AppaBin = if ($env:APPA_BIN) {
    $env:APPA_BIN
} elseif ($env:APPA_INSTALL_DIR) {
    Join-Path $env:APPA_INSTALL_DIR 'appa.exe'
} else {
    Join-Path $env:LOCALAPPDATA 'appa\bin\appa.exe'
}

$AppaDataDir = if ($env:APPA_DATA_DIR) {
    $env:APPA_DATA_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'appa'
}

$AppaConfig = if ($env:APPA_CONFIG) {
    $env:APPA_CONFIG
} elseif ($env:APPA_CONFIG_DIR) {
    Join-Path $env:APPA_CONFIG_DIR 'appa.toml'
} else {
    Join-Path $env:APPDATA 'appa\appa.toml'
}

$AppaEndpoint = if ($env:APPA_ENDPOINT) { $env:APPA_ENDPOINT } else { 'http://127.0.0.1:8787' }
$AppaListen = if ($env:APPA_LISTEN) { $env:APPA_LISTEN } else { '127.0.0.1:8787' }
