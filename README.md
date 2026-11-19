English | [Русский](README.ru.md)

# Proxywatch

Proxywatch gives Linux applications a stable local HTTP or SOCKS5 proxy. It checks whether each upstream can reach the service you use. If the active route fails, new connections use a healthy backup on the same local port.

## Features

- Run HTTP, SOCKS5 (TCP), or both. The default endpoints are `127.0.0.1:10809` for HTTP and `127.0.0.1:10808` for SOCKS5.
- Read VLESS/REALITY/Vision over TCP and Hysteria2 nodes from a Mihomo YAML subscription. You can use a link from Remnawave, Marzban, or 3x-ui (X-UI) when the panel serves a compatible `proxies` list. Setup only needs the subscription URL. If a refresh fails, Proxywatch keeps the last valid list.
- Choose the preferred node by the fastest successful HTTP check. By default, Proxywatch checks the active node every 30 seconds and reserves every 15 minutes. After three consecutive failures it checks the reserves immediately. When the preferred node recovers, new connections return to it. During a total outage forwarding stops. Existing connections may need to reconnect.
- Receive Telegram alerts through the active proxy and email via direct SMTP during a total outage or when Telegram delivery fails. Alerts show UTC and the server’s local time when it differs.
- Inspect node health and event history in the terminal, or read machine-readable status from a script. History and queued alerts are stored locally in SQLite.

## Install or update

Available for Linux x86_64 and ARM64. Run the same command for installation and updates:

```sh
curl -fsSLo install.sh https://github.com/nahashed/proxywatch/releases/latest/download/install.sh &&
  sudo bash install.sh
```

If you are already root, run `bash install.sh` without `sudo`.

The installer checks SHA-256 and uses an existing compatible [Xray-core](https://github.com/XTLS/Xray-core) (26.7.28 or later). If Xray is missing or too old, it installs a verified copy of 26.7.28 in `/usr/local/lib/proxywatch/xray` automatically. The system copy stays available to other applications. On Debian or Ubuntu, the online installer uses APT to install `unzip` if both `unzip` and Python 3 are missing. Use `--non-interactive` for unattended installation. See `bash install.sh --help` for offline installation or a specific version.

Updates preserve settings and the database. An identical release leaves the installed application and its running service untouched. When files change, the installer restarts the invoking account’s active packaged service if available and reports when a manual restart is needed. The primary command is `proxywatch`. The installer adds the shortcut `pwch` when available.

## First run

1. Interactive installation opens setup for the invoking account whenever its settings are missing, including after an interrupted install. You can reopen it with `proxywatch`. Choose a language, name the installation, enter the subscription URL, and choose a URL that your applications need to reach.
2. Choose the local protocols and ports. If another Xray already listens on your preferred port, test Proxywatch on a free port first. To reuse the original address, free that listener before switching. Optionally configure Telegram, SMTP, or subscription device identification.
3. Save the settings. On a system with an available systemd user manager, Proxywatch enables and starts its installed user service. In a console without a user manager (for example, some Proxmox `pct enter` sessions), the installer still completes. After saving, you can start Proxywatch in that terminal. It runs until you press Ctrl+C or close the session. Point your application at the chosen local endpoint.

Reopen `proxywatch` to change settings, view skipped subscription entries, or inspect events. The default data directory is `~/.config/proxywatch` (or `$XDG_CONFIG_HOME/proxywatch`). Use `--directory PATH` for a separate instance.

Under **Checks and intervals**, select additional resources or add your own URLs. Press Enter to keep the suggested interval. For example, set Gemini to `86400` seconds to check it once a day while the primary Telegram check runs every `30` seconds. Additional results appear in the status screen. The primary check controls failover.

### Reusing ports from an existing Xray

Enter the new subscription in setup and select the loopback ports your applications already use. Free ports work alongside your existing service. During first setup as root, an installed Proxywatch can also offer to take over the ports of a dedicated system `xray.service`. The wizard identifies HTTP and SOCKS5 from Xray's running configuration and verifies which process owns the listeners. It keeps each protocol on its original port.

Choose the transfer to test the new subscription through a temporary proxy while Xray continues serving clients. After a successful check, the wizard asks for permission to stop the service and checks again for active connections. There is a short interruption during the switch. If the new route fails its checks on the requested ports, Proxywatch attempts to restore Xray. If ownership is uncertain, the service has other listeners, or the existing endpoints require authentication or UDP, choose different ports or migrate them manually.

The transfer leaves boot settings intact. A user service started this way runs for the current session. To switch back, stop that Proxywatch service and start the system `xray.service`. In foreground transfer mode, Ctrl+C returns the ports to Xray automatically. To transfer them again with saved settings, open `proxywatch` (or `pwch`) and choose **Hand over Xray ports**.

## CLI for scripts and agents

Run these commands as the account that owns the configuration. Restrict access to files containing credentials.

```sh
proxywatch configure --stdin < settings.json
proxywatch configure --stdin --merge < changes.json
proxywatch status --json
```

A minimal settings file is `{"subscription_url":"https://example.invalid/subscription"}`. The first command writes a complete configuration and fills omitted fields with defaults. Use `--merge` to change selected fields in an existing configuration. Nested objects are merged and arrays are replaced. For example, `{"socks_port":null}` disables SOCKS5 while keeping the HTTP settings. Input is validated before saving. Restart a running daemon after changing settings from the CLI.

For providers that require device identification, set `subscription_hwid` to `true` for an automatic ID derived from the Linux machine ID and provider origin, or to a custom string such as `"my-x-hwid-01"`. Automatic IDs are 64 hexadecimal characters (HMAC-SHA256). A custom ID accepts 10 to 64 ASCII letters, digits, hyphens, or equals signs. Use custom values where you have permission to assign the identifier. The default is `false`. When enabled, Proxywatch sends `x-hwid` with HTTPS subscription requests to the configured origin.

In `diagnostic_urls`, a URL string uses the primary check interval. An object with `interval_seconds` sets an interval for that resource. For example, use this JSON with `--merge`.

```json
{"diagnostic_urls":["https://example.org/",{"url":"https://gemini.google.com/","interval_seconds":86400}]}
```

`status --json` returns the latest saved snapshot, including node counts, the last update as a Unix timestamp, and pending alerts. JSON keys, values, and exit codes are independent of the interface language. Exit codes are `0` for success, `1` for a configuration or runtime error, and `2` for invalid command syntax.

## Project

Report bugs and suggest changes in [GitHub Issues](https://github.com/nahashed/proxywatch/issues). Remove subscription URLs, bot tokens, and SMTP passwords from reports.

Licensed under the [GNU Affero General Public License v3.0](LICENSE).
