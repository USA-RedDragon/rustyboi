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

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use rustyboi_session::apply::FetchPurpose;

/// A fetch request: try `urls` in order, tag the result with `purpose`.
struct Job {
    urls: Vec<String>,
    purpose: FetchPurpose,
}

/// A completed fetch, ready to feed back into the session.
pub(crate) struct Finished {
    pub purpose: FetchPurpose,
    /// The URL that produced the body (the first candidate that returned 2xx), or
    /// `None` on failure. Used to name the on-disk cache file for No-Intro DATs.
    pub url: Option<String>,
    /// The response body on success, or an error message.
    pub result: Result<String, String>,
}

/// Owns the background HTTP thread and the channels to it. Created lazily (first
/// fetch), then reused for the process lifetime.
pub(crate) struct FetchWorker {
    tx: Option<Sender<Job>>,
    done_rx: Receiver<Finished>,
    handle: Option<JoinHandle<()>>,
}

impl FetchWorker {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn submit(&mut self, urls: Vec<String>, purpose: FetchPurpose) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job { urls, purpose });
        }
    }

    /// Non-blocking drain of completed fetches.
    pub(crate) fn drain_finished(&mut self) -> Vec<Finished> {
        let mut out = Vec::new();
        // try_recv yields Ok until the queue drains, then an Err (Empty or
        // Disconnected) ends the loop.
        while let Ok(f) = self.done_rx.try_recv() {
            out.push(f);
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
    let mut builder =
        ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(20)));
    // Native-root TLS: the OS trust anchors (Android's X509TrustManager via JNI,
    // the system store on desktop) as rustls root certs, with rustls's own
    // verification. NOT the platform verifier — Android's mandates OCSP and
    // hard-fails on OCSP-less certs like github/Let's Encrypt (rustls-platform-
    // verifier #221). No bundled roots.
    match native_tls_config() {
        Some(cfg) => builder = builder.tls_config(cfg),
        None => log::warn!("cheat-fetch: no native trust roots; TLS will fail"),
    }
    // Bind the process to the active network so native DNS/sockets work from this
    // worker thread (otherwise getaddrinfo fails with EAI_NODATA even online).
    #[cfg(target_os = "android")]
    crate::android::bind_process_to_network();
    let agent: ureq::Agent = builder.build().into();
    while let Ok(job) = rx.recv() {
        let (url, result) = match fetch_first(&agent, &job.urls) {
            Ok((url, body)) => (Some(url), Ok(body)),
            Err(e) => (None, Err(e)),
        };
        if done_tx.send(Finished { purpose: job.purpose, url, result }).is_err() {
            break; // main side gone
        }
    }
}

/// A ureq TLS config trusting the OS's CA roots (no revocation checking), using
/// rustls's own verification against those roots rather than the platform verifier.
fn native_tls_config() -> Option<ureq::tls::TlsConfig> {
    let mut certs: Vec<ureq::tls::Certificate<'static>> = Vec::new();

    #[cfg(target_os = "android")]
    for der in crate::android::system_ca_certs() {
        certs.push(ureq::tls::Certificate::from_der(&der).to_owned());
    }

    #[cfg(not(target_os = "android"))]
    {
        let loaded = rustls_native_certs::load_native_certs();
        for cert in loaded.certs {
            certs.push(ureq::tls::Certificate::from_der(cert.as_ref()).to_owned());
        }
    }

    if certs.is_empty() {
        return None;
    }
    Some(
        ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::new_with_certs(&certs))
            .build(),
    )
}

/// Try each URL in order; return the first 2xx `(url, body)`, else the last error.
/// A 404 falls through to the next candidate (the other system folder).
fn fetch_first(agent: &ureq::Agent, urls: &[String]) -> Result<(String, String), String> {
    let mut last_err = "no URLs to fetch".to_string();
    for url in urls {
        match agent.get(url.as_str()).call() {
            // No-Intro DATs run to a few MB; lift ureq's default 10MB read cap.
            Ok(mut resp) => match resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string() {
                Ok(body) => return Ok((url.clone(), body)),
                Err(e) => last_err = format!("read failed: {e}"),
            },
            Err(ureq::Error::StatusCode(code)) => {
                last_err = format!("HTTP {code}");
                // Non-2xx (e.g. 404): try the next candidate folder.
            }
            Err(e) => last_err = format!("request failed: {e}"),
        }
    }
    // A 404 fallthrough across candidates is normal; only the final failure is
    // surfaced (as a status message by the caller).
    Err(last_err)
}
