use std::path::{Path, PathBuf};

use super::*;
use crate::comms::backend::{CommsBackend, CommsError};

#[test]
fn muxer_comms_backend_default() {
    let backend = MuxerCommsBackend::default();
    assert_eq!(
        backend.socket_dir().to_str().unwrap(),
        MuxerCommsBackend::DEFAULT_SOCKET_DIR
    );
}

#[test]
fn muxer_comms_backend_custom_socket_dir() {
    let backend = MuxerCommsBackend::with_socket_dir(PathBuf::from("/tmp/visor-test"));
    assert_eq!(backend.socket_dir(), Path::new("/tmp/visor-test"));
}

#[test]
fn muxer_socket_path_format() {
    let backend = MuxerCommsBackend::with_socket_dir(PathBuf::from("/tmp/visor-test"));
    assert_eq!(
        backend.muxer_socket_path(7),
        PathBuf::from("/tmp/visor-test/7.sock")
    );
}

#[tokio::test]
async fn connect_to_missing_socket_returns_error() {
    let dir = crate::testutil::tempdir("visor-vmm-muxer-").unwrap();
    let backend = MuxerCommsBackend::with_socket_dir(dir.path().to_path_buf());

    let result = backend.connect(3, 52).await;
    let Err(error) = result else {
        panic!("connecting to a missing muxer socket should fail");
    };
    assert!(matches!(
        error,
        CommsError::Connect {
            cid: 3,
            port: 52,
            ..
        }
    ));
}

#[tokio::test]
async fn connect_to_muxer_socket_succeeds() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = crate::testutil::tempdir("visor-vmm-muxer-").unwrap();
    let cid = 9_u32;
    let port = 52_u32;
    let listener = tokio::net::UnixListener::bind(dir.path().join(format!("{cid}.sock"))).unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim(), format!("CONNECT {port}"));
        stream.write_all(b"OK 1073741824\n").await.unwrap();

        let mut payload = [0u8; 5];
        stream.read_exact(&mut payload).await.unwrap();
        stream.write_all(&payload).await.unwrap();
    });

    let backend = MuxerCommsBackend::with_socket_dir(dir.path().to_path_buf());
    let mut stream = backend
        .connect(cid, port)
        .await
        .expect("connect should succeed");
    stream.write_all(b"visor").await.unwrap();
    let mut echoed = [0u8; 5];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"visor");
}
