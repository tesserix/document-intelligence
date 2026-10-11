use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    process::ExitCode,
    time::Duration,
};

fn healthy(address: SocketAddr) -> io::Result<bool> {
    let timeout = Duration::from_secs(1);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut status = Vec::new();
    BufReader::new(stream)
        .take(128)
        .read_until(b'\n', &mut status)?;
    Ok(status.ends_with(b"\r\n")
        && (status.starts_with(b"HTTP/1.1 200 ") || status.starts_with(b"HTTP/1.0 200 ")))
}

fn main() -> ExitCode {
    if healthy(SocketAddr::from(([127, 0, 0, 1], 8080))).unwrap_or(false) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    #[test]
    fn only_a_successful_http_liveness_response_is_healthy() {
        for (response, expected) in [
            ("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", true),
            ("HTTP/1.1 503 Unavailable\r\n\r\n", false),
            ("HTTP/1.1 2000 Invalid\r\n\r\n", false),
            ("not HTTP\r\n", false),
            ("", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0; 256];
                let length = stream.read(&mut request).unwrap();
                assert!(request[..length].starts_with(b"GET /healthz HTTP/1.1\r\n"));
                stream.write_all(response.as_bytes()).unwrap();
            });
            assert_eq!(healthy(address).unwrap_or(false), expected, "{response}");
            server.join().unwrap();
        }
    }
}
