//! Off-thread HTTP(S) GET for the libretro cheat-DB fetch (native desktop +
//! Android).
//!
//! The UI thread must never block on the network. This worker mirrors
//! [`RewindWorker`](crate::rewind_worker): a background thread receives fetch
//! jobs, performs a blocking `ureq` GET (rustls TLS, no OpenSSL), and hands the
//! result back over an mpsc channel that the platform loop drains once per frame.
//!
//! Each job carries an ordered list of candidate URLs (the cheat DB occasionally
//! misfiles an entry across the GB/GBC folders); the worker tries them in order
//! and returns the first 2xx body, or the last error.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use rustyboi_session::apply::FetchPurpose;

/// ureq DNS resolver for Android. The device's system resolver can be unhealthy
/// even when the network works (e.g. a Private DNS / DNS-over-TLS server that the
/// browser sidesteps with its own DoH): try the JVM `InetAddress` resolver first,
/// then fall back to DNS-over-HTTPS via 1.1.1.1 (an IP literal — needs no system
/// DNS at all).
#[cfg(target_os = "android")]
struct AndroidResolver {
    doh: std::sync::Arc<ureq::Agent>,
}

#[cfg(target_os = "android")]
impl ureq::Resolver for AndroidResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        use std::io::{Error, ErrorKind};
        let (host, port) = netloc
            .rsplit_once(':')
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing port"))?;
        let port: u16 = port
            .parse()
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "bad port"))?;
        let ips = crate::android::resolve_host(host).or_else(|e| {
            log::warn!("cheat-fetch: system DNS failed ({e}); falling back to DoH");
            doh_resolve(&self.doh, host)
        })?;
        Ok(ips
            .into_iter()
            .map(|ip| std::net::SocketAddr::new(ip, port))
            .collect())
    }
}

/// A ureq agent for DNS-over-HTTPS queries to 1.1.1.1. It uses no custom resolver
/// because the endpoint is an IP literal, so it never touches the system DNS.
#[cfg(target_os = "android")]
fn doh_agent() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(10));
    if let Ok(cfg) = {
        use rustls_platform_verifier::ConfigVerifierExt;
        rustls::ClientConfig::with_platform_verifier()
    } {
        b = b.tls_config(std::sync::Arc::new(cfg));
    }
    b.build()
}

/// Resolve `host` to IPs via Cloudflare DoH (JSON API) over `1.1.1.1`.
#[cfg(target_os = "android")]
fn doh_resolve(agent: &ureq::Agent, host: &str) -> std::io::Result<Vec<std::net::IpAddr>> {
    use std::io::{Error, ErrorKind};
    let mut ips = Vec::new();
    for qtype in ["A", "AAAA"] {
        let url = format!("https://1.1.1.1/dns-query?name={host}&type={qtype}");
        let body = match agent.get(&url).set("accept", "application/dns-json").call() {
            Ok(r) => r.into_string().unwrap_or_default(),
            Err(e) => {
                log::warn!("cheat-fetch: DoH {qtype} query failed: {e}");
                continue;
            }
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        if let Some(answers) = json.get("Answer").and_then(|a| a.as_array()) {
            for a in answers {
                if let Some(ip) = a
                    .get("data")
                    .and_then(|d| d.as_str())
                    .and_then(|d| d.parse::<std::net::IpAddr>().ok())
                {
                    ips.push(ip);
                }
            }
        }
    }
    if ips.is_empty() {
        return Err(Error::new(ErrorKind::Other, "DoH returned no addresses"));
    }
    Ok(ips)
}

/// A fetch request: try `urls` in order, tag the result with `purpose`.
struct Job {
    urls: Vec<String>,
    purpose: FetchPurpose,
}

/// A completed fetch, ready to feed back into the session.
pub struct Finished {
    pub purpose: FetchPurpose,
    /// The response body on success, or an error message.
    pub result: Result<String, String>,
}

/// Owns the background HTTP thread and the channels to it. Created lazily (first
/// fetch), then reused for the process lifetime.
pub struct FetchWorker {
    tx: Option<Sender<Job>>,
    done_rx: Receiver<Finished>,
    handle: Option<JoinHandle<()>>,
}

impl FetchWorker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Finished>();
        let handle = std::thread::Builder::new()
            .name("cheat-fetch".to_string())
            .spawn(move || fetch_loop(rx, done_tx))
            .expect("spawn cheat-fetch thread");
        FetchWorker { tx: Some(tx), done_rx, handle: Some(handle) }
    }

    /// Enqueue a fetch. Cheap on the caller — it only moves the URLs into the
    /// channel.
    pub fn submit(&mut self, urls: Vec<String>, purpose: FetchPurpose) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job { urls, purpose });
        }
    }

    /// Non-blocking drain of completed fetches.
    pub fn drain_finished(&mut self) -> Vec<Finished> {
        let mut out = Vec::new();
        loop {
            match self.done_rx.try_recv() {
                Ok(f) => out.push(f),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

impl Drop for FetchWorker {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn fetch_loop(rx: Receiver<Job>, done_tx: Sender<Finished>) {
    use rustls_platform_verifier::ConfigVerifierExt;
    // Native-root TLS via the OS verifier: the system trust store on desktop, the
    // Android CA store (through the JNI init in android_main) on Android. No
    // bundled roots. Install the ring provider so ClientConfig::builder() has one.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20));
    match rustls::ClientConfig::with_platform_verifier() {
        Ok(cfg) => builder = builder.tls_config(std::sync::Arc::new(cfg)),
        Err(e) => log::warn!("cheat-fetch: platform TLS verifier config failed: {e}"),
    }
    // Android's system resolver is unreliable from worker threads; resolve DNS
    // via the JVM, falling back to DNS-over-HTTPS.
    #[cfg(target_os = "android")]
    {
        builder = builder.resolver(AndroidResolver {
            doh: std::sync::Arc::new(doh_agent()),
        });
    }
    let agent = builder.build();
    while let Ok(job) = rx.recv() {
        let result = fetch_first(&agent, &job.urls);
        if done_tx.send(Finished { purpose: job.purpose, result }).is_err() {
            break; // main side gone
        }
    }
}

/// Try each URL in order; return the first 2xx body, else the last error. A 404
/// falls through to the next candidate (the other system folder).
fn fetch_first(agent: &ureq::Agent, urls: &[String]) -> Result<String, String> {
    let mut last_err = "no URLs to fetch".to_string();
    for url in urls {
        match agent.get(url).call() {
            Ok(resp) => match resp.into_string() {
                Ok(body) => return Ok(body),
                Err(e) => last_err = format!("read failed: {e}"),
            },
            Err(ureq::Error::Status(code, _)) => {
                last_err = format!("HTTP {code}");
                // Non-2xx (e.g. 404): try the next candidate folder.
            }
            Err(e) => last_err = format!("request failed: {e}"),
        }
        // The status toast truncates; log the full per-URL error to logcat.
        log::warn!("cheat fetch: {url} -> {last_err}");
    }
    Err(last_err)
}
