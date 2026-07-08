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
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut builder = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20));
    // Native-root TLS: the OS trust anchors (Android's X509TrustManager via JNI,
    // the system store on desktop) loaded into a rustls root store, with rustls's
    // own verification. NOT the platform verifier — Android's mandates OCSP and
    // hard-fails on OCSP-less certs like github/Let's Encrypt (rustls-platform-
    // verifier #221). No bundled roots.
    match native_tls_config() {
        Some(cfg) => builder = builder.tls_config(std::sync::Arc::new(cfg)),
        None => log::warn!("cheat-fetch: no native trust roots; TLS will fail"),
    }
    // Bind the process to the active network so native DNS/sockets work from this
    // worker thread (otherwise getaddrinfo fails with EAI_NODATA even online).
    #[cfg(target_os = "android")]
    crate::android::bind_process_to_network();
    let agent = builder.build();
    while let Ok(job) = rx.recv() {
        let result = fetch_first(&agent, &job.urls);
        if done_tx.send(Finished { purpose: job.purpose, result }).is_err() {
            break; // main side gone
        }
    }
}

/// A rustls client config trusting the OS's CA roots (no revocation checking).
fn native_tls_config() -> Option<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();

    #[cfg(target_os = "android")]
    for der in crate::android::system_ca_certs() {
        let _ = roots.add(rustls::pki_types::CertificateDer::from(der));
    }

    #[cfg(not(target_os = "android"))]
    {
        let loaded = rustls_native_certs::load_native_certs();
        for cert in loaded.certs {
            let _ = roots.add(cert);
        }
    }

    if roots.is_empty() {
        return None;
    }
    Some(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
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
