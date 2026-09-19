#![cfg(windows)]
use app_proxy_core::{model::*, singbox};
use app_proxy_windows::store::Store;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Core(Child);
impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => panic!("fixture accept failed: {e}"),
        }
    }
}
fn header(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 8192 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|_| std::io::ErrorKind::InvalidData.into())
}
fn request(port: u16) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(1),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(
        b"CONNECT route-proof.invalid:443 HTTP/1.1\r\nHost: route-proof.invalid:443\r\n\r\n",
    )?;
    let response = header(&mut stream)?;
    if !response.starts_with("HTTP/1.1 200") {
        return Err(std::io::ErrorKind::ConnectionRefused.into());
    }
    let mut marker = [0; 7];
    stream.read_exact(&mut marker)?;
    Ok(String::from_utf8(marker.to_vec()).unwrap())
}
fn profile(port: u16, upstream: u16, protocol: ManualProtocol, username: &str) -> ProxyProfile {
    let node = ManualNode {
        id: Uuid::new_v4(),
        name: "fixture".into(),
        protocol,
        host: "127.0.0.1".into(),
        port: upstream,
        credentials: Some(Credentials {
            username: username.into(),
            password_secret_id: Uuid::new_v4(),
        }),
    };
    ProxyProfile {
        id: Uuid::new_v4(),
        name: "fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port,
        },
        selected_node_id: node.id,
        source: ProxySource::Manual { nodes: vec![node] },
    }
}

#[test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX pointing to the verified development-only sing-box executable"]
fn generated_config_routes_two_authenticated_upstreams_in_one_real_process() {
    let binary = PathBuf::from(
        std::env::var_os("APP_PROXY_TEST_SING_BOX").expect("explicit validation binary required"),
    );
    let version = Command::new(&binary)
        .arg("version")
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("sing-box version 1.14.1"));
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = http.local_addr().unwrap().port();
    let socks = TcpListener::bind("127.0.0.1:0").unwrap();
    let socks_port = socks.local_addr().unwrap().port();
    let reserved: Vec<_> = (0..3)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<_> = reserved
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owned");
    let store = Store::create(&root).unwrap();
    let mut manifest = store.load().unwrap();
    manifest.profiles = vec![
        profile(ports[0], http_port, ManualProtocol::Http, "alice"),
        profile(ports[1], socks_port, ManualProtocol::Socks5, "bob"),
    ];
    let ids: Vec<_> = manifest.profiles.iter().map(|p| p.id).collect();
    let compiled = singbox::compile(&manifest, &ids, |_| Ok("secret".into())).unwrap();
    let config = root.join("core.json");
    fs::write(&config, compiled.bytes()).unwrap();
    assert!(
        Command::new(&binary)
            .args(["check", "-c"])
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    // A test-only unmatched inbound exercises the compiler's final rejection.
    let mut value: serde_json::Value = serde_json::from_slice(compiled.bytes()).unwrap();
    value["inbounds"].as_array_mut().unwrap().push(serde_json::json!({"type":"http","tag":"unmatched-fixture","listen":"127.0.0.1","listen_port":ports[2],"set_system_proxy":false}));
    fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    drop(reserved);
    let mut core = Core(
        Command::new(&binary)
            .args(["run", "-c"])
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            core.0.try_wait().unwrap().is_none(),
            "own core exited during startup"
        );
        if ports
            .iter()
            .all(|port| TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, *port)).is_ok())
        {
            break;
        }
        assert!(Instant::now() < deadline, "listeners did not become ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let http_task = std::thread::spawn(move || {
        let mut connection = accept(&http);
        let observed = header(&mut connection).unwrap();
        assert!(observed.starts_with("CONNECT route-proof.invalid:443 "));
        assert!(
            observed
                .to_ascii_lowercase()
                .contains("proxy-authorization: basic ywxpy2u6c2vjcmv0")
        );
        connection
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nproof-A")
            .unwrap();
    });
    let socks_task = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut connection = accept(&socks);
            let mut greeting = [0; 2];
            connection.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0; greeting[1] as usize];
            connection.read_exact(&mut methods).unwrap();
            assert!(methods.contains(&2));
            connection.write_all(&[5, 2]).unwrap();
            let mut auth = [0; 2];
            connection.read_exact(&mut auth).unwrap();
            assert_eq!(auth[0], 1);
            let mut username = vec![0; auth[1] as usize];
            connection.read_exact(&mut username).unwrap();
            assert_eq!(username, b"bob");
            let mut length = [0];
            connection.read_exact(&mut length).unwrap();
            let mut password = vec![0; length[0] as usize];
            connection.read_exact(&mut password).unwrap();
            assert_eq!(password, b"secret");
            connection.write_all(&[1, 0]).unwrap();
            let mut connect = [0; 5];
            connection.read_exact(&mut connect).unwrap();
            assert_eq!(&connect[..4], &[5, 1, 0, 3]);
            let mut domain = vec![0; connect[4] as usize];
            connection.read_exact(&mut domain).unwrap();
            assert_eq!(domain, b"route-proof.invalid");
            let mut port = [0; 2];
            connection.read_exact(&mut port).unwrap();
            assert_eq!(u16::from_be_bytes(port), 443);
            connection
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                .unwrap();
            connection.write_all(b"proof-B").unwrap();
        }
    });
    assert!(
        request(ports[2]).is_err(),
        "unmatched inbound must not use the first outbound"
    );
    assert_eq!(request(ports[0]).unwrap(), "proof-A");
    http_task.join().unwrap(); // First upstream now closed.
    assert_eq!(request(ports[1]).unwrap(), "proof-B");
    assert!(
        request(ports[0]).is_err(),
        "failed A must not fall back to live B"
    );
    assert_eq!(request(ports[1]).unwrap(), "proof-B");
    socks_task.join().unwrap();
    assert!(core.0.try_wait().unwrap().is_none());
    drop(core);
    drop(store);
}
