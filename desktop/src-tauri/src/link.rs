//! "Use my website passkey": hands the passkey wallet of the website to this app without typing seed words.
//!
//! The wallet's derivation is the website's (web/src/lib/wallet/passkey.js, frozen) and needs WebAuthn, which a webview of this app
//! cannot offer for the website's relying party. So the system browser does it:
//!   1. the app opens a one-time listener on 127.0.0.1 (random port), a random secret and an ephemeral X25519 key, and opens
//!      `https://sm64coin.pages.dev/desktop-link#p=<port>&s=<secret>&k=<app public key>`;
//!   2. that page asks for the passkey, derives the wallet (seed words), makes its own ephemeral X25519 key and POSTs
//!      {secret, pk, iv, ct}: ct = AES-256-GCM(HKDF-SHA256(X25519(shared), salt = app pk || page pk, info = "sm64coin desktop-link v1"),
//!      JSON {seed, height, address});
//!   3. the app checks the secret (constant time), the Origin (the website; `http://localhost:5173` only in debug builds) and the
//!      Host (127.0.0.1:<port>, against DNS rebinding), decrypts, keeps the seed in memory, and closes the listener. The page
//!      answers Chrome's Private Network Access preflight (`Access-Control-Allow-Private-Network: true`).
//!   4. the UI shows the address that arrived and asks the user to compare it with the one the browser page shows; only then it
//!      calls `link_finish` with the wallet name and a password chosen in the app: the seed is restored through wallet-rpc (like
//!      "restore from seed") and wiped. The restored wallet's address must be the one that was shown, else it is not kept.
//! Not in any log: the secret, the seed. The listener lives at most 5 minutes, takes one good POST, and gives up after 10 bad ones;
//! a received seed is kept (zeroized on drop) for at most 10 minutes.
//!
//! Why the address check: the URL (with the secret) is an argument of the browser process (xdg-open/open/rundll32), which other
//! local processes can read, and a local process can send any Origin. Such a process could win the race with a seed of its own
//! (it cannot read the real one: that is encrypted to the app's key). It cannot make the app show the user's own address for it:
//! the address is checked against the restored seed.
use crate::desktop::App;
use crate::{AppError, Res};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State as TState};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

pub const SITE: &str = "https://sm64coin.pages.dev";
const INFO: &[u8] = b"sm64coin desktop-link v1";
const LIFETIME: Duration = Duration::from_secs(300);
const MAX_BAD: u32 = 10;
const MAX_BODY: usize = 16 * 1024;
/// How long a received seed waits for `link_finish`.
const RECEIVED_TTL: Duration = Duration::from_secs(600);

/// Origins the page may POST from. The dev server is allowed in debug builds only.
fn origin_ok(o: &str) -> bool {
    o == SITE || (cfg!(debug_assertions) && o == "http://localhost:5173")
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Idle,
    Waiting,
    Received,
    Expired,
    Cancelled,
    Failed,
}

pub struct Received {
    pub seed: Zeroizing<String>,
    pub height: u64,
    pub address: String,
    at: Instant,
}

#[derive(Default)]
pub struct Link {
    inner: Arc<Mutex<Inner>>,
}
struct Inner {
    state: LinkState,
    /// Bumped on every start: an old listener thread must not touch a newer link.
    epoch: u64,
    got: Option<Received>,
    stop: bool,
    error: Option<String>,
}
impl Default for Inner {
    fn default() -> Self {
        Inner { state: LinkState::Idle, epoch: 0, got: None, stop: false, error: None }
    }
}

// ---- crypto (also what the page does, see web/src/lib/desktopLink.js) ----

fn key_for(my_secret: &StaticSecret, my_pk: &PublicKey, their_pk: &[u8; 32]) -> Result<[u8; 32], String> {
    let theirs = PublicKey::from(*their_pk);
    let shared = my_secret.diffie_hellman(&theirs);
    if !shared.was_contributory() {
        return Err("bad public key".into());
    }
    // Same salt order on both sides: app public key first.
    let (app, page) = (my_pk.as_bytes(), their_pk);
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(app);
    salt.extend_from_slice(page);
    let mut k = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes()).expand(INFO, &mut k).map_err(|e| e.to_string())?;
    Ok(k)
}

/// The app's side: decrypts what the page sent. `pk`: the page's public key.
pub fn open_box(app_secret: &StaticSecret, pk: &[u8; 32], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>, String> {
    let app_pk = PublicKey::from(app_secret);
    let key = key_for(app_secret, &app_pk, pk)?;
    if iv.len() != 12 {
        return Err("bad iv".into());
    }
    Aes256Gcm::new_from_slice(&key).unwrap().decrypt(Nonce::from_slice(iv), Payload { msg: ct, aad: INFO }).map_err(|_| "could not decrypt".to_string())
}

/// What the page does (kept here for the round-trip test): encrypts to the app's public key.
#[cfg(test)]
fn seal_box(app_pk: &[u8; 32], page_secret: &StaticSecret, iv: &[u8; 12], plain: &[u8]) -> Vec<u8> {
    let page_pk = PublicKey::from(page_secret);
    let shared = page_secret.diffie_hellman(&PublicKey::from(*app_pk));
    let mut salt = Vec::new();
    salt.extend_from_slice(app_pk);
    salt.extend_from_slice(page_pk.as_bytes());
    let mut k = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes()).expand(INFO, &mut k).unwrap();
    Aes256Gcm::new_from_slice(&k).unwrap().encrypt(Nonce::from_slice(iv), Payload { msg: plain, aad: INFO }).unwrap()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn rand32() -> [u8; 32] {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).expect("the system random generator is unavailable");
    b
}

#[derive(Deserialize)]
struct Posted {
    secret: String,
    pk: String,
    iv: String,
    ct: String,
}

#[derive(Deserialize)]
struct Payload_ {
    seed: String,
    #[serde(default)]
    height: u64,
    address: String,
}
impl Drop for Payload_ {
    fn drop(&mut self) {
        self.seed.zeroize();
    }
}

/// A primary address: base58, about 95 characters (the restore checks it against the seed).
fn plausible_address(a: &str) -> bool {
    (90..=110).contains(&a.len()) && a.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Validates a request and, for a good POST, returns what the page sent. Err(status, text) for a refusal.
fn handle_post(body: &[u8], secret: &str, app_secret: &StaticSecret) -> Result<Received, (u16, &'static str)> {
    let p: Posted = serde_json::from_slice(body).map_err(|_| (400, "bad request"))?;
    if !ct_eq(p.secret.as_bytes(), secret.as_bytes()) {
        return Err((403, "wrong secret"));
    }
    let dec = |s: &str| B64.decode(s).map_err(|_| (400u16, "bad encoding"));
    let pk: [u8; 32] = dec(&p.pk)?.try_into().map_err(|_| (400, "bad key"))?;
    let plain = Zeroizing::new(open_box(app_secret, &pk, &dec(&p.iv)?, &dec(&p.ct)?).map_err(|_| (400, "could not decrypt"))?);
    let v: Payload_ = serde_json::from_slice(&plain).map_err(|_| (400, "bad payload"))?;
    let words = v.seed.split_whitespace().count();
    if words != 25 {
        return Err((400, "not a 25-word seed"));
    }
    if !plausible_address(&v.address) {
        return Err((400, "no wallet address"));
    }
    let mut seed = Zeroizing::new(String::with_capacity(v.seed.len()));
    for (i, w) in v.seed.split_whitespace().enumerate() {
        if i > 0 {
            seed.push(' ');
        }
        seed.push_str(w);
    }
    Ok(Received { seed, height: v.height, address: v.address.clone(), at: Instant::now() })
}

fn cors(req_origin: &str) -> Vec<tiny_http::Header> {
    let h = |k: &str, v: &str| tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap();
    vec![
        h("Access-Control-Allow-Origin", req_origin),
        h("Vary", "Origin"),
        h("Access-Control-Allow-Methods", "POST, OPTIONS"),
        h("Access-Control-Allow-Headers", "content-type"),
        h("Access-Control-Allow-Private-Network", "true"),
        h("Access-Control-Max-Age", "60"),
        h("Cache-Control", "no-store"),
    ]
}

fn serve(inner: Arc<Mutex<Inner>>, epoch: u64, server: tiny_http::Server, port: u16, secret: String, app_secret: StaticSecret) {
    let t0 = Instant::now();
    let mut bad = 0u32;
    let set = |f: &dyn Fn(&mut Inner)| {
        let mut g = inner.lock().unwrap();
        if g.epoch == epoch {
            f(&mut g);
        }
    };
    loop {
        {
            let g = inner.lock().unwrap();
            if g.epoch != epoch || g.stop {
                return;
            }
        }
        if t0.elapsed() > LIFETIME {
            set(&|g| g.state = LinkState::Expired);
            return;
        }
        let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(500)) else { continue };
        let header = |req: &tiny_http::Request, n: &str| req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(n)).map(|h| h.value.as_str().to_string());
        let origin = header(&req, "Origin").unwrap_or_default();
        let host_ok = header(&req, "Host").is_some_and(|h| h == format!("127.0.0.1:{port}"));
        let reply = |req: tiny_http::Request, code: u16, text: &str, o: Option<&str>| {
            let mut r = tiny_http::Response::from_string(text).with_status_code(code);
            for h in o.map(cors).unwrap_or_default() {
                r.add_header(h);
            }
            let _ = req.respond(r);
        };
        if !host_ok || !origin_ok(&origin) {
            bad += 1;
            reply(req, 403, "forbidden", None);
        } else if req.method() == &tiny_http::Method::Options {
            reply(req, 204, "", Some(&origin));
        } else if req.method() == &tiny_http::Method::Post && req.url() == "/link" {
            let mut body = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(req.as_reader(), MAX_BODY as u64 + 1), &mut body);
            if body.len() > MAX_BODY {
                bad += 1;
                reply(req, 413, "too large", Some(&origin));
            } else {
                match handle_post(&body, &secret, &app_secret) {
                    Ok(got) => {
                        reply(req, 200, "{\"ok\":true}", Some(&origin));
                        let mut g = inner.lock().unwrap();
                        if g.epoch == epoch && !g.stop {
                            g.got = Some(got);
                            g.state = LinkState::Received;
                        }
                        return; // one good POST, then the listener is gone
                    }
                    Err((c, t)) => {
                        bad += 1;
                        reply(req, c, t, Some(&origin));
                    }
                }
            }
        } else {
            bad += 1;
            reply(req, 404, "not found", Some(&origin));
        }
        if bad >= MAX_BAD {
            set(&|g| {
                g.state = LinkState::Failed;
                g.error = Some("too many bad requests to the one-time listener".into());
            });
            return;
        }
    }
}

#[derive(Serialize)]
pub struct LinkInfo {
    state: LinkState,
    /// The page to open (only while waiting; the app opens it itself).
    url: Option<String>,
    address: Option<String>,
    error: Option<String>,
}

/// Drops a received seed nobody finished in time.
fn expire(g: &mut Inner) {
    if g.got.as_ref().is_some_and(|r| r.at.elapsed() > RECEIVED_TTL) {
        g.got = None;
        g.state = LinkState::Expired;
    }
}

fn info(l: &Link, url: Option<String>) -> LinkInfo {
    let mut g = l.inner.lock().unwrap();
    expire(&mut g);
    LinkInfo { state: g.state, url, address: g.got.as_ref().map(|r| r.address.clone()).filter(|a| !a.is_empty()), error: g.error.clone() }
}

pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut c = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32"); // no shell: the fragment has '&'
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ()).map_err(|e| format!("cannot open the browser: {e}"))
}

/// Starts the one-time listener and opens the website's link page in the system browser (`open: false` only returns the URL).
#[tauri::command]
pub fn link_start(app: TState<'_, App>, open: Option<bool>) -> Res<LinkInfo> {
    let l = &app.link;
    let (epoch, inner) = {
        let mut g = l.inner.lock().unwrap();
        g.stop = true; // an older listener ends
        g.epoch += 1;
        g.state = LinkState::Waiting;
        g.got = None;
        g.stop = false;
        g.error = None;
        (g.epoch, l.inner.clone())
    };
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| AppError::new("link_listen", e.to_string()))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).ok_or_else(|| AppError::new("link_listen", "no port"))?;
    let secret = B64.encode(&rand32()[..16]);
    let app_secret = StaticSecret::from(rand32());
    let pk = B64.encode(PublicKey::from(&app_secret).as_bytes());
    let base = if cfg!(debug_assertions) { std::env::var("SM64COIN_LINK_SITE").unwrap_or_else(|_| SITE.to_string()) } else { SITE.to_string() };
    let url = format!("{}/desktop-link#p={port}&s={secret}&k={pk}", base.trim_end_matches('/'));
    let s2 = secret.clone();
    std::thread::spawn(move || serve(inner, epoch, server, port, s2, app_secret));
    #[cfg(debug_assertions)]
    if let Ok(f) = std::env::var("SM64COIN_LINK_URL_FILE") {
        let _ = std::fs::write(f, &url); // tests only (debug builds): the secret is otherwise never written anywhere
    }
    // If no browser can be opened the link is still shown for the user to copy.
    if open.unwrap_or(true) {
        if let Err(e) = open_browser(&url) {
            l.inner.lock().unwrap().error = Some(format!("{e}; copy the link below into your browser"));
        }
    }
    Ok(info(l, Some(url)))
}

#[tauri::command]
pub fn link_status(app: TState<'_, App>) -> Res<LinkInfo> {
    Ok(info(&app.link, None))
}

#[tauri::command]
pub fn link_cancel(app: TState<'_, App>) -> Res<LinkInfo> {
    {
        let mut g = app.link.inner.lock().unwrap();
        g.stop = true;
        g.got = None;
        g.state = LinkState::Cancelled;
    }
    Ok(info(&app.link, None))
}

/// Imports the received wallet under `name` with the password the user chose in the app (seed restore through wallet-rpc).
/// `address`: the address the user confirmed (the one `link_status` showed); the restored wallet must have it.
#[tauri::command]
pub async fn link_finish(app: AppHandle, name: String, password: String, address: String, kdf_rounds: Option<u32>) -> Res<Value> {
    let password = Zeroizing::new(password);
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<App>();
        let got = {
            let mut g = st.link.inner.lock().unwrap();
            expire(&mut g);
            if !g.got.as_ref().is_some_and(|r| r.address == address) {
                return Err(AppError::new("link_nothing", "no passkey wallet with that address was received"));
            }
            g.state = LinkState::Idle;
            g.got.take().unwrap()
        };
        let r = crate::wallet::restore_from_seed(&app, &name, &password, &got.seed, got.height, kdf_rounds, Some(&got.address));
        if let Err(e) = &r {
            // Not imported (a bad name, a weak password, the sidecar): keep the seed for another try, but not a seed that does
            // not belong to the address the user confirmed.
            if e.code != "address_mismatch" {
                let mut g = st.link.inner.lock().unwrap();
                if g.got.is_none() && g.state == LinkState::Idle {
                    g.got = Some(got);
                    g.state = LinkState::Received;
                }
            }
        }
        r
    })
    .await
    .map_err(|e| AppError::new("internal", e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_to_app_round_trip() {
        let app = StaticSecret::from([7u8; 32]);
        let page = StaticSecret::from([9u8; 32]);
        let app_pk = *PublicKey::from(&app).as_bytes();
        let page_pk = *PublicKey::from(&page).as_bytes();
        let iv = [3u8; 12];
        const ADDR: &str = "SM64xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let plain = format!(r#"{{"seed":"a b c d e f g h i j k l m n o p q r s t u v w x y","height":42,"address":"{ADDR}"}}"#);
        let plain = plain.as_bytes();
        let ct = seal_box(&app_pk, &page, &iv, plain);
        assert_eq!(open_box(&app, &page_pk, &iv, &ct).unwrap(), plain);
        // a payload without an address is refused (the user compares it, the restore checks it)
        let ct3 = seal_box(&app_pk, &page, &iv, br#"{"seed":"a b c d e f g h i j k l m n o p q r s t u v w x y"}"#);
        let body3 = serde_json::json!({"secret": "s3cret", "pk": B64.encode(page_pk), "iv": B64.encode(iv), "ct": B64.encode(&ct3)}).to_string();
        assert_eq!(handle_post(body3.as_bytes(), "s3cret", &app).err().map(|e| e.0), Some(400));
        // a flipped bit, another key, another iv: all refused
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert!(open_box(&app, &page_pk, &iv, &bad).is_err());
        assert!(open_box(&StaticSecret::from([8u8; 32]), &page_pk, &iv, &ct).is_err());
        assert!(open_box(&app, &page_pk, &[4u8; 12], &ct).is_err());
        // the whole POST: secret, base64url fields, 25 words
        let body = serde_json::json!({"secret": "s3cret", "pk": B64.encode(page_pk), "iv": B64.encode(iv), "ct": B64.encode(&ct)}).to_string();
        let got = handle_post(body.as_bytes(), "s3cret", &app).unwrap();
        assert_eq!((got.height, got.address.as_str(), got.seed.split(' ').count()), (42, ADDR, 25));
        assert_eq!(handle_post(body.as_bytes(), "other", &app).err().map(|e| e.0), Some(403));
        // a 24-word payload is not a seed
        let ct2 = seal_box(&app_pk, &page, &iv, br#"{"seed":"a b c"}"#);
        let body2 = serde_json::json!({"secret": "s3cret", "pk": B64.encode(page_pk), "iv": B64.encode(iv), "ct": B64.encode(&ct2)}).to_string();
        assert_eq!(handle_post(body2.as_bytes(), "s3cret", &app).err().map(|e| e.0), Some(400));
    }

    #[test]
    fn origins() {
        assert!(origin_ok("https://sm64coin.pages.dev"));
        assert!(!origin_ok("https://sm64coin.pages.dev.evil.example"));
        assert!(!origin_ok("http://sm64coin.pages.dev"));
        assert!(!origin_ok(""));
        assert_eq!(origin_ok("http://localhost:5173"), cfg!(debug_assertions));
    }
}
