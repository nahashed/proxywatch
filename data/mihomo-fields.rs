pub(super) const COMMON: &[&str] = &["name", "type", "server", "port", "udp", "smux"];

pub(super) const DISABLED_ONLY: &[&str] = &["tfo", "mptcp", "skip-cert-verify"];

pub(super) const VLESS: &[&str] = &[
    "uuid",
    "tls",
    "network",
    "reality-opts",
    "encryption",
    "flow",
    "servername",
    "client-fingerprint",
    "packet-encoding",
];

pub(super) const REALITY: &[&str] = &["public-key", "short-id", "support-x25519mlkem768"];

pub(super) const HYSTERIA2: &[&str] = &[
    "password",
    "sni",
    "alpn",
    "obfs",
    "obfs-password",
    "fingerprint",
    "up",
    "down",
    "bbr-profile",
];
