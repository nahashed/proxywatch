English | [Русский](README.ru.md)

# Proxywatch

Give your application one local HTTP or SOCKS5 endpoint. If its upstream stops reaching the service you need, Proxywatch routes new connections through a working backup on the same port. If every node fails, it stops forwarding rather than sending traffic directly.

Proxywatch runs on Linux. It uses [Xray](https://github.com/XTLS/Xray-core) for proxy connections and reads Mihomo YAML subscriptions. It does not need a panel API or admin token.

## Features

- Run an HTTP proxy, a SOCKS5 TCP proxy, or both on separate loopback ports.
- Check the target service through each node. After a confirmed failure, send new connections to a working reserve on the same local port. Return to the preferred node when it recovers.
- Import VLESS/REALITY/Vision over TCP and Hysteria2 from a Mihomo YAML `proxies` list. View the reason for each skipped entry in the menu. A failed refresh preserves the last valid subscription. Providers that require device identification can receive an optional HWID.
- Send Telegram alerts through the active proxy and use direct SMTP for a total outage or failed Telegram delivery. Menus and alerts support English and Russian.
- Configure from JSON, read status from scripts and keep node history and queued notifications locally in SQLite.

## Install

The Linux binaries target x86_64 and aarch64 with glibc and are tested on Debian 12. Install Xray separately (tested with 26.7.28). Set its absolute path in the setup menu if it is not on `PATH`. For a public release, download the installer and run it:

```sh
curl -fsSLo install.sh https://github.com/nahashed/proxywatch/releases/latest/download/install.sh &&
  sudo bash install.sh
```

The installer selects the latest stable release for your Linux architecture, checks the archive against its SHA-256 manifest and installs `proxywatch`. It also creates the `pwch` shortcut if that command is free. It does not reconfigure Xray or start a service.

To update, run `sudo bash install.sh` again. If the installed files match the verified release, the installer exits successfully without replacing them. An update preserves your settings and SQLite database. Restart any running Proxywatch process to use the new binary. For a systemd user service, run `systemctl --user daemon-reload && systemctl --user restart proxywatch.service` as the service owner.

For a private release or an offline machine, use the installer from a local checkout and the archive and `SHA256SUMS` from the trusted release. With both assets in `./release`, run `sudo bash install.sh --from ./release --version v0.1.0`. Before a release is available, build from the checkout with `cargo build --release --locked` and run `./target/release/proxywatch`.

## First run

1. Run `proxywatch` as the account that will run the service. On first setup, choose English or Русский. Name the installation, enter the subscription URL and choose a URL that must be reachable through the proxy.
2. Choose HTTP, SOCKS5 (TCP), or both on separate ports. Defaults are `127.0.0.1:10809` for HTTP and `127.0.0.1:10808` for SOCKS5. Choose unused ports if another proxy is already running.
3. Save the settings, then run `proxywatch run` in the foreground or enable the systemd user service:

```sh
systemctl --user daemon-reload
systemctl --user enable --now proxywatch.service
```

Point your application at the local port. Reopen `proxywatch` to see the active route, node health and event history. Restart the daemon after changing settings. To keep a user service running after logout, enable lingering for that account.

## Automation

Run these commands as the service owner. Supply a settings object on standard input. The file can contain credentials, so restrict its permissions. A minimal example with a placeholder subscription URL is:

```json
{"subscription_url":"https://example.invalid/subscription"}
```

```sh
proxywatch configure --stdin < settings.json
proxywatch status --json
```

For a single listener, set the unused port to `null` in the JSON settings. For example, `"socks_port": null` leaves only HTTP enabled. Omitting a port uses its default.

If your subscription provider requires a device ID, enable HWID in subscription settings or set `"subscription_hwid": true`. Proxywatch sends an `x-hwid` header on HTTPS subscription requests to the configured origin. It derives a stable ID from the Linux machine ID and the provider’s origin. The ID stays the same for a given installation, host name and HTTPS port. Changing the Linux machine ID changes the HWID. Sending it is disabled by default.

Configuration replaces the entire settings object. Omitted fields take their defaults. Unknown fields are rejected. Validation happens before saving, and successful configuration produces no output. Restart a running daemon to apply the settings.

Status reports the last saved snapshot. It includes a schema version, the configuration flag, the recorded service state, the Unix update timestamp, whether the preferred node is active, and counts for nodes and queued notifications. It also works before the first run. Exit codes are 0 for success, 1 for an operation or configuration error, and 2 for invalid command syntax.

Before setup, both languages return this status:

```json
{"schema_version":1,"configured":false,"status":"unknown","updated_at":0,"active_is_preferred":null,"nodes_total":0,"nodes_available":0,"pending_notifications":0}
```

Set `"language": "en"` or `"language": "ru"` in the settings object to select the language for menus, CLI messages and alerts. You can also change it in Settings. Existing configurations default to Russian. The status JSON and exit codes are identical in both languages.

## What happens when a node fails?

Suppose node A is preferred and B is on standby. By default, Proxywatch checks A every 30 seconds. After three consecutive failures it tries the reserves. If B passes the same check, **new connections use B**. Your application keeps its proxy address, though existing connections may need to reconnect. Once A passes again, **new connections return to A**, even if B is healthy.

- **Selection:** the first scan prefers the fastest successful HTTP response through a node (2xx or 3xx), not the lowest ICMP ping. The preference survives restarts. Reserves are checked every 15 minutes by default and on failover. Additional diagnostic URLs do not trigger switches.
- **Local data:** each installation keeps its history and notification queue in SQLite under `~/.config/proxywatch` (or `$XDG_CONFIG_HOME/proxywatch`). The same directory holds subscription and notification credentials. Keep it private. Use `--directory PATH` for a separate instance.

## Project

Maintainer: [@nahashed](https://github.com/nahashed). Report bugs or suggest changes in [GitHub Issues](https://github.com/nahashed/proxywatch/issues). Do not include subscription URLs, bot tokens or SMTP passwords in reports.

License: [GNU Affero General Public License v3.0](LICENSE).
