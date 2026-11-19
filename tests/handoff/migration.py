"""Drive the real first-run TUI inside the disposable container from handoff-linux.sh."""
import contextlib
from collections import Counter
import errno
import fcntl
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import pty
import pwd
import select
import shutil
import signal
import socket
import ssl
import struct
import sys
import subprocess
import termios
import threading
import time

APP = "/usr/local/bin/proxywatch"
XRAY = "/usr/local/bin/xray"
DIRECT, UPSTREAM, HYSTERIA = 18109, 18110, 28443
HTTP_PORT, SOCKS_PORT = 10808, 10809  # Deliberately reversed from the Proxywatch defaults.
SETTINGS = Path("/root/.config/proxywatch")
CONFIG = Path("/usr/local/etc/xray/config.json")
UNIT = Path("/etc/systemd/system/xray.service")
TARGET = f"http://127.0.0.1:{DIRECT}/health"


def run(*args, **kwargs):
    return subprocess.run(args, capture_output=True, check=True, text=True, timeout=25, **kwargs).stdout.strip()


def property_of(unit, key):
    return run("systemctl", "show", "--property=" + key, "--value", unit)


def enabled(unit, user=False):
    args = ["systemctl"] + (["--user"] if user else [])
    return subprocess.run(args + ["is-enabled", unit], capture_output=True, text=True, timeout=5).stdout.strip()


def wait_for(label, predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if predicate():
                return
        except (OSError, http.client.HTTPException):
            pass
        time.sleep(.1)
    raise AssertionError(f"Timed out waiting for {label}")


def connect_socks(client):
    client.sendall(b"\x05\x01\x00")
    if client.recv(2, socket.MSG_WAITALL) != b"\x05\x00":
        return False
    client.sendall(b"\x05\x01\x00\x01\x7f\x00\x00\x01" + struct.pack("!H", DIRECT))
    return client.recv(10, socket.MSG_WAITALL)[:4] == b"\x05\x00\x00\x01"


def through(protocol, port):
    with socket.create_connection(("127.0.0.1", port), timeout=2) as client:
        target = TARGET
        if protocol == "socks":
            if not connect_socks(client):
                return None
            target = "/health"
        client.sendall(f"GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{DIRECT}\r\nConnection: close\r\n\r\n".encode())
        response = http.client.HTTPResponse(client)
        response.begin()
        return response.read() if response.status == 200 else None


def verify_traffic(expected, http_port=HTTP_PORT, socks_port=SOCKS_PORT):
    for protocol, port in [("http", http_port), ("socks", socks_port)]:
        wait_for(f"{protocol} port {port} serves {expected!r}", lambda: through(protocol, port) == expected)


class Fixture(http.server.BaseHTTPRequestHandler):
    requests = Counter()

    def do_GET(self):
        self.requests[self.server.server_port, self.path] += 1
        if self.path == "/sub":
            body = self.server.profile
        elif self.server.server_port == UPSTREAM:
            body = b"upstream"
        else:
            body = b"direct"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def start_fixtures(stack):
    cert_prefix = "/tmp/handoff-cert"
    run(XRAY, "tls", "cert", "-domain=localhost", "-file=" + cert_prefix)
    cert = Path(cert_prefix + ".crt")
    pin = hashlib.sha256(ssl.PEM_cert_to_DER_cert(cert.read_text())).hexdigest()
    for port in [DIRECT, UPSTREAM]:
        server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Fixture)
        server.profile = json.dumps({"proxies": [{"type": "hysteria2", "name": "local fixture",
            "server": "127.0.0.1", "port": HYSTERIA, "password": "fixture-only",
            "sni": "localhost", "alpn": ["h3"], "fingerprint": pin}]}).encode()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.server_close)
        stack.callback(server.shutdown)
    config = Path("/tmp/handoff-upstream.json")
    config.write_text(json.dumps({
        "inbounds": [{"listen": "127.0.0.1", "port": HYSTERIA, "protocol": "hysteria",
            "settings": {"version": 2, "users": [{"auth": "fixture-only"}]},
            "streamSettings": {"network": "hysteria", "security": "tls", "hysteriaSettings": {"version": 2},
                "tlsSettings": {"alpn": ["h3"], "certificates": [{"certificateFile": str(cert),
                    "keyFile": cert_prefix + ".key"}]}}}],
        "outbounds": [{"protocol": "freedom", "settings": {"redirect": f"127.0.0.1:{UPSTREAM}",
            "finalRules": [{"action": "allow", "ip": ["127.0.0.1/32"], "port": str(UPSTREAM)}]}}]}))
    return config


def start_legacy(extra=False, foreign=False, restart_condition=None):
    def inbound(protocol, port):
        options = {"auth": "noauth", "udp": False} if protocol == "socks" else {}
        return {"listen": "127.0.0.1", "port": port, "protocol": protocol, "settings": options}
    inputs = [inbound("socks", SOCKS_PORT)]
    if not foreign:
        inputs.append(inbound("http", HTTP_PORT))
    if extra:
        inputs.append(inbound("http", 10910))
    CONFIG.parent.mkdir(parents=True, exist_ok=True)
    CONFIG.write_text(json.dumps({"inbounds": inputs, "outbounds": [{"protocol": "freedom",
        "settings": {"finalRules": [{"action": "allow", "ip": ["127.0.0.1/32"], "port": str(DIRECT)}]}}]}))
    condition = f"ConditionPathExists={restart_condition}\n" if restart_condition else ""
    UNIT.write_text("[Unit]\nDescription=Isolated Xray proxy\n" + condition + "[Service]\n"
        f"User=nobody\nExecStart={XRAY} run -config {CONFIG}\n[Install]\nWantedBy=multi-user.target\n")
    run("systemctl", "daemon-reload")
    run("systemctl", "enable", "--now", "xray.service")
    process = Path("/proc") / property_of("xray.service", "MainPID")
    assert os.path.samefile(process / "exe", XRAY)
    assert process.stat().st_uid == pwd.getpwnam("nobody").pw_uid
    assert list((process / "fd").iterdir())
    if not foreign:
        verify_traffic(b"direct")
    else:
        wait_for("legacy SOCKS", lambda: through("socks", SOCKS_PORT) == b"direct")


class Wizard:
    def __enter__(self):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.execve(APP, [APP, "tui"], dict(os.environ, PROXYWATCH_INSTALL_SETUP="1",
                LANG="C.UTF-8", TERM="xterm-256color", HOME="/root"))
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 180, 0, 0))
        self.pending = bytearray()
        self.log = bytearray()
        return self

    def __exit__(self, kind, *_):
        if kind:
            print(self.log[-1700:].decode(errors="replace"), flush=True)
            print(f"Local fixture requests: {dict(Fixture.requests)}", flush=True)
        if self.pid:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.fd)

    def read(self, timeout):
        if select.select([self.fd], [], [], timeout)[0]:
            try:
                data = os.read(self.fd, 65536)
            except OSError as exc:
                if exc.errno == errno.EIO:
                    return False
                raise
            self.log.extend(data)
            self.pending.extend(data)
            return bool(data)
        return True

    def wait(self, label, timeout=35):
        key = label.encode()
        deadline = time.monotonic() + timeout
        while key not in self.pending:
            if label == "Temporarily stop xray.service" and b"No subscription node passed" in self.pending:
                raise AssertionError("The isolated subscription failed the preflight")
            if time.monotonic() > deadline or not self.read(.2):
                raise AssertionError(f"Wizard did not reach {label!r}")
        del self.pending[:self.pending.index(key) + len(key)]

    def send(self, keys):
        # Dialoguer changes terminal mode while moving between prompts.
        time.sleep(.08)
        if keys.startswith(("http://", "https://")):
            os.write(self.fd, keys.encode())
        else:
            for key in keys:
                os.write(self.fd, key.encode())
                time.sleep(.025)

    def choose(self, label, keys="\r"):
        self.wait(label)
        self.send(keys)

    def first_save(self):
        self.choose("Русский", "k\r")
        self.choose("Mihomo YAML subscription URL", f"http://127.0.0.1:{DIRECT}/sub\r")
        for label in ["Use a custom HWID", "Installation name", "Purpose (optional)", "HTTP only",
                      "SOCKS5 port", "HTTP port"]:
            self.choose(label)
        self.choose("Custom URL", "jjjjjj\r")
        self.choose("Health check URL", TARGET + "\r")
        for label in ["Active node check interval", "Backup node check interval", "Consecutive failures",
                      "Request timeout", "Subscription refresh interval", "Choose resources for additional checks through the proxy?",
                      "Email over a direct SMTP connection", "Exit without saving"]:
            self.choose(label)


def active(unit, user=False):
    args = ["systemctl"] + (["--user"] if user else [])
    return subprocess.run(args + ["is-active", "--quiet", unit], timeout=5).returncode == 0


def reset():
    for unit, user in [("proxywatch.service", True), ("xray.service", False)]:
        base = ["systemctl"] + (["--user"] if user else [])
        subprocess.run(base + ["disable", "--now", unit], capture_output=True, timeout=20)
        subprocess.run(base + ["reset-failed", unit], capture_output=True, timeout=5)
    if SETTINGS.exists():
        shutil.rmtree(SETTINGS)


@contextlib.contextmanager
def packaged_system_unit():
    fragment = Path("/usr/local/lib/systemd/system/proxywatch.service")
    run("install", "-Dm644", "/fixture/proxywatch-system.service", str(fragment))
    run("systemctl", "daemon-reload")
    try:
        yield
    finally:
        run("systemctl", "disable", "--now", "proxywatch.service")
        fragment.unlink()
        run("systemctl", "daemon-reload")


def verify_system_setup(name):
    if name == "system-saved":
        # A configuration written by 0.1.1 while the root user manager was unavailable.
        SETTINGS.mkdir(mode=0o700, parents=True)
        (SETTINGS / "settings.json").write_text(json.dumps({
            "language": "en", "instance_name": "Existing root setup",
            "subscription_url": f"http://127.0.0.1:{DIRECT}/sub", "check_url": TARGET,
            "diagnostic_urls": [], "xray_binary": XRAY,
        }))
        with Wizard() as wizard:
            wizard.choose("Close menu", "jjj\r")
            wizard.choose("Exit without saving")
            wizard.wait("Enable and start the Proxywatch system service")
            with socket.socket() as late_listener:
                late_listener.bind(("127.0.0.1", SOCKS_PORT))
                late_listener.listen()
                wizard.send("\r")
                wizard.wait("The service or ports changed")
                assert not active("proxywatch.service")
                assert enabled("proxywatch.service") == "disabled"
                assert late_listener.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN)
    with Wizard() as wizard:
        if name == "system-first-save":
            wizard.first_save()
        else:
            wizard.choose("Close menu", "jjj\r")
            wizard.choose("Exit without saving")
            wizard.choose("Enable and start the Proxywatch system service")
        wizard.wait("The Proxywatch system service is enabled and running")
    # The terminal is closed. The system service still owns both configured listeners.
    assert enabled("proxywatch.service") == "enabled"
    assert active("proxywatch.service")
    assert not active("user@0.service")
    settings = json.loads((SETTINGS / "settings.json").read_bytes())
    verify_traffic(b"upstream", settings["http_port"], settings["socks_port"])


def scenario(name, upstream_config):
    reset()
    Fixture.requests.clear()
    with contextlib.ExitStack() as stack:
        upstream = subprocess.Popen([XRAY, "run", "-config", str(upstream_config)])
        def upstream_ready():
            if upstream.poll() is not None:
                raise AssertionError(f"Hysteria2 fixture exited with status {upstream.returncode}")
            for table in ("udp", "udp6"):
                for row in Path("/proc/net", table).read_text().splitlines()[1:]:
                    if int(row.split()[1].split(":")[1], 16) == HYSTERIA:
                        return True
            return False
        def stop_upstream():
            if upstream.poll() is None:
                upstream.terminate()
                upstream.wait(timeout=5)
        stack.callback(stop_upstream)
        wait_for("local Hysteria2 fixture to listen", upstream_ready)
        if name in ("foreground", "manager-change") or name.startswith("system-"):
            run("systemctl", "stop", "user@0.service")
            stack.callback(run, "systemctl", "start", "user@0.service")
        if name.startswith("system-"):
            stack.enter_context(packaged_system_unit())
        if name in ("system-first-save", "system-saved"):
            verify_system_setup(name)
            print(f"PASS system service: {name}", flush=True)
            return
        foreign = name == "foreign"
        if foreign:
            other = stack.enter_context(socket.socket())
            other.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            other.bind(("127.0.0.1", HTTP_PORT))
            other.listen(8)
        condition = Path("/run/legacy-xray-available") if name == "restore-failed" else None
        if condition:
            condition.touch()
        start_legacy(extra=name == "extra", foreign=foreign, restart_condition=condition)
        original = property_of("xray.service", "InvocationID")
        old_files = (CONFIG.read_bytes(), UNIT.read_bytes())
        if name == "busy":
            connection = stack.enter_context(socket.create_connection(("127.0.0.1", SOCKS_PORT), timeout=2))
            assert connect_socks(connection)

        with Wizard() as wizard:
            wizard.first_save()
            wizard.wait("Local ports are in use")
            wizard.wait("Return to settings")
            takeover = "Test the subscription for handover"
            if name in ("busy", "extra", "foreign"):
                # Leave the conflict screen before inspecting its fully rendered choice list.
                wizard.send("jj\r")
                wizard.wait("Exit without saving")
                assert takeover.encode() not in wizard.log
                assert property_of("xray.service", "InvocationID") == original
                assert property_of("xray.service", "ActiveState") == "active"
                assert not (SETTINGS / "settings.json").exists()
                if name == "busy":
                    assert b"xray.service has active connections" in wizard.log
                    connection.sendall(b"GET /health HTTP/1.0\r\n\r\n")
                    reply = http.client.HTTPResponse(connection)
                    reply.begin()
                    assert reply.read() == b"direct"
                if foreign:
                    assert other.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN) == 1
            elif name == "probe-failure":
                stop_upstream()
                wizard.choose(takeover, "jjj\r")
                wizard.wait("No subscription node passed")
                assert property_of("xray.service", "InvocationID") == original
                assert not (SETTINGS / "settings.json").exists()
                verify_traffic(b"direct")
            else:
                wizard.choose(takeover, "jjj\r")
                wizard.wait("Temporarily stop xray.service")
                assert property_of("xray.service", "InvocationID") == original
                assert Fixture.requests[DIRECT, "/sub"] and Fixture.requests[UPSTREAM, "/health"]
                if name == "manager-change":
                    assert b"Proxywatch will run in this terminal" in wizard.log
                    run("systemctl", "start", "user@0.service")
                if name == "system-handover":
                    assert b"Proxywatch will run as the system service" in wizard.log
                if name == "late-client":
                    connection = stack.enter_context(socket.create_connection(("127.0.0.1", SOCKS_PORT), timeout=2))
                    assert connect_socks(connection)
                if name in ("rollback", "restore-failed"):
                    stop_upstream()
                if condition:
                    condition.unlink()
                wizard.send("k\r")
                if name == "manager-change":
                    wizard.wait("The port owner, connections or settings changed.")
                    assert property_of("xray.service", "InvocationID") == original
                    assert not (SETTINGS / "settings.json").exists()
                    verify_traffic(b"direct")
                elif name == "late-client":
                    wizard.wait("The port owner, connections or settings changed.")
                    assert property_of("xray.service", "InvocationID") == original
                    assert not (SETTINGS / "settings.json").exists()
                    connection.sendall(b"GET /health HTTP/1.0\r\n\r\n")
                    reply = http.client.HTTPResponse(connection)
                    reply.begin()
                    assert reply.read() == b"direct"
                elif name == "restore-failed":
                    wait_for("replacement started", lambda: active("proxywatch.service", user=True))
                    wizard.send("\x03")
                    wizard.wait("Could not confirm that the original service was restored.")
                    assert not active("xray.service") and not active("proxywatch.service", user=True)
                    assert not (SETTINGS / "settings.json").exists()
                elif name == "rollback":
                    wait_for("rollback restarts the original instance",
                             lambda: property_of("xray.service", "InvocationID") not in ("", original)
                             and property_of("xray.service", "ActiveState") == "active", 85)
                    wizard.wait("The original Xray service is active.", 15)
                    verify_traffic(b"direct")
                    assert not active("proxywatch.service", user=True)
                    assert not (SETTINGS / "settings.json").exists()
                else:
                    if name == "foreground":
                        wizard.wait("Press Ctrl+C to return them to Xray.")
                    else:
                        scope = "system" if name == "system-handover" else "user"
                        wizard.wait(f"The Proxywatch {scope} service is running")
                    # Probing earlier would itself create a client during the ownership recheck.
                    verify_traffic(b"upstream")
                    assert property_of("xray.service", "ActiveState") == "inactive"
                    if name == "foreground":
                        wizard.send("\x03")
                        wizard.wait("The original Xray service is active again.")
                        verify_traffic(b"direct")
                    else:
                        user = name != "system-handover"
                        assert active("proxywatch.service", user=user)
                        assert enabled("proxywatch.service", user=user) == "disabled"
                    settings = json.loads((SETTINGS / "settings.json").read_bytes())
                    assert (settings["http_port"], settings["socks_port"]) == (HTTP_PORT, SOCKS_PORT)
        if name == "foreground":
            saved = (SETTINGS / "settings.json").read_bytes()
            restored = property_of("xray.service", "InvocationID")
            with Wizard() as wizard:
                wizard.choose("Hand over Xray ports", "jjjj\r")
                wizard.choose("Temporarily stop xray.service", "\r")
                wizard.wait("Close menu")
                assert (SETTINGS / "settings.json").read_bytes() == saved
                assert property_of("xray.service", "InvocationID") == restored
        assert enabled("xray.service") == "enabled"
        assert (CONFIG.read_bytes(), UNIT.read_bytes()) == old_files
        print(f"PASS handover: {name}", flush=True)


def main():
    # Service creation and cleanup must be confined to this test's disposable container.
    if (not __debug__ or os.environ.get("PROXYWATCH_HANDOFF_TEST") != "1"
            or not Path("/.dockerenv").is_file() or os.geteuid() != 0
            or Path("/proc/1/comm").read_text().strip() != "systemd"):
        raise SystemExit("Run this test in the disposable container from handoff-linux.sh")
    interfaces = {name for _, name in socket.if_nameindex()
                  if int(Path(f"/sys/class/net/{name}/flags").read_text(), 16) & 1}
    if interfaces != {"lo"}:
        raise SystemExit("The test requires a loopback-only network")
    with contextlib.ExitStack() as stack:
        config = start_fixtures(stack)
        scenarios = ("busy", "foreign", "extra", "probe-failure", "late-client", "success", "rollback", "restore-failed", "foreground",
                     "manager-change", "system-handover", "system-first-save", "system-saved")
        for name in sys.argv[1:] or scenarios:
            if name not in scenarios:
                raise ValueError(f"Unknown scenario: {name}")
            scenario(name, config)


if __name__ == "__main__":
    main()
