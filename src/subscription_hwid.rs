use crate::runtime_text::message;
use crate::subscription_identity::HwidMode;
use anyhow::{Context, Result, ensure};
use reqwest::{
    Url,
    blocking::{Client, RequestBuilder},
    header::HeaderValue,
};
use ring::hmac;
use std::{fmt::Write, fs, io};
use zeroize::Zeroizing;

pub fn redirect_policy(enabled: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if allows_redirect(attempt.url(), attempt.previous(), enabled) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn allows_redirect(next: &Url, previous: &[Url], enabled: bool) -> bool {
    previous.len() <= 4
        && matches!(next.scheme(), "https" | "http")
        && next.username().is_empty()
        && next.password().is_none()
        && !(next.scheme() == "http" && previous.iter().any(|url| url.scheme() == "https"))
        && (!enabled
            || previous
                .first()
                .is_some_and(|source| next.scheme() == "https" && next.origin() == source.origin()))
}

pub fn request(client: &Client, url: Url, mode: &HwidMode) -> Result<RequestBuilder> {
    match mode {
        HwidMode::Toggle(false) => Ok(client.get(url)),
        HwidMode::Custom(value) => request_with_header(client, url, value),
        HwidMode::Toggle(true) => {
            prepare(client, url, true, || fs::read_to_string("/etc/machine-id"))
        }
    }
}

fn prepare(
    client: &Client,
    url: Url,
    enabled: bool,
    read_machine_id: impl FnOnce() -> io::Result<String>,
) -> Result<RequestBuilder> {
    if !enabled {
        return Ok(client.get(url));
    }
    ensure!(url.scheme() == "https", message!(hwid_https));
    let machine_id = Zeroizing::new(read_machine_id().context(message!(hwid_unavailable))?);
    let id = machine_id.trim_end_matches('\n');
    ensure!(
        id.len() == 32
            && id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            && id.bytes().any(|c| c != b'0'),
        message!(hwid_unavailable)
    );

    // Scope the pseudonym to the application and origin, independent of subscription tokens.
    let key = hmac::Key::new(hmac::HMAC_SHA256, id.as_bytes());
    let mut context = hmac::Context::with_key(&key);
    context.update(b"proxywatch/subscription-hwid/v1\0");
    context.update(url.origin().ascii_serialization().as_bytes());
    let tag = context.sign();
    let mut hex = String::with_capacity(tag.as_ref().len() * 2);
    for byte in tag.as_ref() {
        write!(hex, "{byte:02x}")?;
    }
    request_with_header(client, url, &hex)
}

fn request_with_header(client: &Client, url: Url, value: &str) -> Result<RequestBuilder> {
    let mut header = HeaderValue::from_str(value)?;
    header.set_sensitive(true);
    Ok(client.get(url).header("x-hwid", header))
}

#[cfg(test)]
#[path = "../tests/unit/subscription_hwid.rs"]
mod tests;
