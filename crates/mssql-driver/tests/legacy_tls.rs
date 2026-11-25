//! Legacy TLS against a local stand-in for SQL Server 2014: TDS pre-login,
//! a TLS 1.0-only handshake carried in pre-login packets, then LOGIN7 over
//! TLS, answered with a login error. Its certificate is self-signed, like
//! SQL Server's default one.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use mssql_driver::MssqlClient;
use openssl::asn1::Asn1Time;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::ssl::{SslAcceptor, SslMethod, SslVersion};
use openssl::x509::{X509NameBuilder, X509};
use ora_core::{ConnectionProfile, DbClient, DbKind};

const PRELOGIN: u8 = 0x12;
const REPLY: u8 = 0x04;
const LOGIN_MESSAGE: &str = "Login failed for user 'kilat'.";

fn packet(kind: u8, payload: &[u8]) -> Vec<u8> {
    let len = u16::try_from(payload.len() + 8).unwrap();
    let mut p = vec![kind, 1];
    p.extend_from_slice(&len.to_be_bytes());
    p.extend_from_slice(&[0, 0, 1, 0]);
    p.extend_from_slice(payload);
    p
}

fn read_packet(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 8];
    r.read_exact(&mut header)?;
    let len = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let mut body = vec![0u8; len - 8];
    r.read_exact(&mut body)?;
    Ok((header[0], body))
}

/// The server's pre-login answer: version 12.0.4100, encryption on.
fn prelogin_reply() -> Vec<u8> {
    let mut p = vec![0x00, 0, 11, 0, 6, 0x01, 0, 17, 0, 1, 0xff];
    p.extend_from_slice(&[12, 0, 0x10, 0x04, 0, 0]);
    p.push(0x01);
    p
}

/// An ERROR token (18456, as for a failed SQL login) and a final DONE.
fn login_error() -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let mut body = Vec::new();
    body.extend_from_slice(&18456i32.to_le_bytes());
    body.extend_from_slice(&[1, 14]);
    body.extend_from_slice(&u16::try_from(LOGIN_MESSAGE.len()).unwrap().to_le_bytes());
    body.extend_from_slice(&utf16(LOGIN_MESSAGE));
    body.push(4);
    body.extend_from_slice(&utf16("fake"));
    body.push(0);
    body.extend_from_slice(&1i32.to_le_bytes());
    let mut p = vec![0xAA];
    p.extend_from_slice(&u16::try_from(body.len()).unwrap().to_le_bytes());
    p.extend_from_slice(&body);
    p.push(0xFD);
    p.extend_from_slice(&2u16.to_le_bytes());
    p.extend_from_slice(&0u16.to_le_bytes());
    p.extend_from_slice(&0u64.to_le_bytes());
    p
}

/// The TCP stream under the server's TLS: handshake records travel in
/// pre-login packets, later ones as they are.
struct Wire {
    tcp: TcpStream,
    handshake: Arc<AtomicBool>,
    pending: Vec<u8>,
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.handshake.load(Ordering::SeqCst) {
            return self.tcp.read(buf);
        }
        if self.pending.is_empty() {
            let (kind, body) = read_packet(&mut self.tcp)?;
            assert_eq!(kind, PRELOGIN, "handshake outside a pre-login packet");
            self.pending = body;
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.handshake.load(Ordering::SeqCst) {
            self.tcp.write_all(&packet(PRELOGIN, buf))?;
        } else {
            self.tcp.write_all(buf)?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

fn acceptor() -> SslAcceptor {
    let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", "localhost").unwrap();
    let name = name.build();
    let mut cert = X509::builder().unwrap();
    cert.set_version(2).unwrap();
    cert.set_subject_name(&name).unwrap();
    cert.set_issuer_name(&name).unwrap();
    cert.set_pubkey(&key).unwrap();
    cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    let cert = cert.build();

    let mut a = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    a.set_private_key(&key).unwrap();
    a.set_certificate(&cert).unwrap();
    a.set_security_level(0);
    a.set_cipher_list("DEFAULT:@SECLEVEL=0").unwrap();
    a.set_min_proto_version(Some(SslVersion::TLS1)).unwrap();
    a.set_max_proto_version(Some(SslVersion::TLS1)).unwrap();
    a.build()
}

struct Server {
    port: u16,
    /// Per connection: the TLS version, once a LOGIN7 arrived over TLS.
    logins: Arc<Mutex<Vec<String>>>,
}

fn serve(acceptor: &SslAcceptor, tcp: TcpStream, logins: &Mutex<Vec<String>>) {
    let mut tcp = tcp;
    let Ok((PRELOGIN, _)) = read_packet(&mut tcp) else {
        return;
    };
    if tcp.write_all(&packet(REPLY, &prelogin_reply())).is_err() {
        return;
    }
    let handshake = Arc::new(AtomicBool::new(true));
    let wire = Wire {
        tcp,
        handshake: handshake.clone(),
        pending: Vec::new(),
    };
    let Ok(mut tls) = acceptor.accept(wire) else {
        return;
    };
    handshake.store(false, Ordering::SeqCst);
    let Ok((kind, _)) = read_packet(&mut tls) else {
        return;
    };
    assert_eq!(kind, 0x10, "LOGIN7 expected");
    logins
        .lock()
        .unwrap()
        .push(tls.ssl().version_str().to_string());
    let _ = tls.write_all(&packet(REPLY, &login_error()));
    let _ = tls.flush();
}

fn start() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let logins = Arc::new(Mutex::new(Vec::new()));
    let acceptor = Arc::new(acceptor());
    {
        let logins = logins.clone();
        thread::spawn(move || {
            for tcp in listener.incoming().flatten() {
                let (acceptor, logins) = (acceptor.clone(), logins.clone());
                thread::spawn(move || serve(&acceptor, tcp, &logins));
            }
        });
    }
    Server { port, logins }
}

fn profile(port: u16, legacy_tls: bool, trust_cert: bool) -> ConnectionProfile {
    let mut p = ConnectionProfile {
        name: "t".into(),
        kind: DbKind::Mssql,
        username: "kilat".into(),
        connect_string: format!("127.0.0.1:{port}/master"),
        ..ConnectionProfile::default()
    };
    p.access.trust_server_certificate = trust_cert;
    p.access.allow_legacy_tls = legacy_tls;
    p
}

/// The steps of setting up an unpatched SQL Server 2014: each refusal names
/// the option that gets past it, and with both the login goes over TLS 1.0.
#[test]
fn legacy_tls_and_trust_reach_login_on_a_tls10_only_server() {
    let server = start();
    let mut client = MssqlClient::new();

    // TLS 1.2 is the default: refused, pointing at the legacy option.
    let err = client
        .connect(&profile(server.port, false, true), "p")
        .unwrap_err();
    assert!(err.0.contains("Allow legacy TLS"), "{err}");

    // TLS 1.0 now, but the self-signed certificate is still checked.
    let err = client
        .connect(&profile(server.port, true, false), "p")
        .unwrap_err();
    assert!(err.0.contains("certificate verify failed"), "{err}");
    assert!(err.0.contains("tick Trust server certificate"), "{err}");
    assert!(!err.0.contains("Allow legacy TLS"), "{err}");
    assert!(server.logins.lock().unwrap().is_empty());

    // With both, the login is sent encrypted with TLS 1.0.
    let err = client
        .connect(&profile(server.port, true, true), "p")
        .unwrap_err();
    assert!(err.0.contains(LOGIN_MESSAGE), "{err}");
    assert_eq!(*server.logins.lock().unwrap(), vec!["TLSv1".to_string()]);
}
