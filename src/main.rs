use std::error::Error;
use std::io::ErrorKind;
use std::sync::{Arc, Mutex};
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
mod recorder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("Server listening on 127.0.0.1:8080");
    loop {
        let (socket, addr) = listener.accept().await?;
        let addr_copy = addr; // Make a copy for error reporting
        tokio::spawn(async move {
            if let Err(e) = handle_client(socket).await {
                eprintln!("Error handling client from {}: {}", addr_copy, e);
            }
        });
    }
}

async fn send_error(
    client_stream: &mut TcpStream,
    code: u32,
    body: &'static str,
) -> io::Result<()> {
    let response = format!("HTTP/1.1 {code} Connection Established\r\n\r\n{body}");
    client_stream.write_all(response.as_bytes()).await?;
    Ok(())
}

struct HttpReader {
    buf: Vec<u8>,
}

struct GetLineResult(usize, String);

fn get_line_fro_vec(buf: &Vec<u8>) -> io::Result<GetLineResult> {
    let n = match buf.windows(2).position(|window| window == [b'\r', b'\n']) {
        Some(n) => n,
        None => return Ok(GetLineResult(0, "".to_string())),
    };
    let str = std::str::from_utf8(&buf[0..n])
        .map_err(|e| io::Error::new(ErrorKind::InvalidData, format!("Invalid UTF-8: {}", e)))?;
    return Ok(GetLineResult(n + 2, str.to_string()));
}

impl HttpReader {
    pub fn new() -> Self {
        Self { buf: vec![] }
    }
    pub async fn read_lines(&mut self, client_stream: &mut TcpStream) -> io::Result<String> {
        loop {
            match get_line_fro_vec(&self.buf) {
                Ok(GetLineResult(0, _)) => (),
                Ok(GetLineResult(n, line)) => {
                    println!("{}", line);
                    self.buf.drain(0..n);
                    return Ok(line);
                }
                Err(e) => return Err(e),
            };
            let begin = self.buf.len();
            self.buf.resize(begin + 4096, 0);
            let n = client_stream.read(&mut self.buf[begin..]).await?;
            self.buf.truncate(self.buf.len() - (begin + n));
        }
    }
}

async fn forward_streams(
    mut client_stream: TcpStream,
    mut target_stream: TcpStream,
) -> io::Result<()> {
    let (mut client_reader, mut client_writer) = client_stream.split();
    let (mut target_reader, mut target_writer) = target_stream.split();

    let client_to_server_recorder = Arc::new(Mutex::new(recorder::Recorder::new()));
    let mut client_to_server_recorder_writer = recorder::RecorderWriter {
        recorder: client_to_server_recorder.clone(),
    };
    let mut client_to_server_recorder_reader =
        recorder::RecorderReader::new(client_to_server_recorder.clone());

    let server_to_client_recorder = Arc::new(Mutex::new(recorder::Recorder::new()));
    let mut server_to_client_recorder_writer = recorder::RecorderWriter {
        recorder: server_to_client_recorder.clone(),
    };
    let mut server_to_client_recorder_reader =
        recorder::RecorderReader::new(server_to_client_recorder.clone());

    let client_to_proxy = async {
        io::copy(&mut client_reader, &mut client_to_server_recorder_writer).await?;
        Ok::<(), io::Error>(())
    };
    let proxy_to_target = async {
        io::copy(&mut client_to_server_recorder_reader, &mut target_writer).await?;
        Ok::<(), io::Error>(())
    };

    let target_to_proxy = async {
        io::copy(&mut target_reader, &mut server_to_client_recorder_writer).await?;
        Ok::<(), io::Error>(())
    };
    let proxy_to_client = async {
        io::copy(&mut server_to_client_recorder_reader, &mut client_writer).await?;
        Ok::<(), io::Error>(())
    };
    tokio::try_join!(
        client_to_proxy,
        proxy_to_target,
        target_to_proxy,
        proxy_to_client
    )?;
    Ok(())
}

async fn handle_client(mut client_stream: TcpStream) -> io::Result<()> {
    let mut reader = HttpReader::new();
    let connect_line = match reader.read_lines(&mut client_stream).await {
        Ok(line) => line,
        Err(e) => {
            send_error(&mut client_stream, 400, "Bad Request").await?;
            return Err(e);
        }
    };
    dbg!(&connect_line);

    if connect_line.starts_with("CONNECT ") {
        let parts: Vec<&str> = connect_line.split_whitespace().collect();
        if parts.len() != 3 || parts[2] != "HTTP/1.1" {
            send_error(&mut client_stream, 400, "Bad Request").await?;
            return Ok(());
        }
        loop {
            let line = reader.read_lines(&mut client_stream).await?;
            if line.is_empty() {
                break;
            } else {
                dbg!(&line);
            }
        }

        let host_port = parts[1];
        let mut host_port_parts = host_port.split(':');
        let host = host_port_parts.next().unwrap_or("");
        let port_str = host_port_parts.next().unwrap_or("443");
        let port: u16 = port_str
            .parse()
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "Invalid port"))?;
        let target_stream = TcpStream::connect((host, port)).await?;
        println!("Connected to target: {}:{}, sending 200 OK", host, port);

        let response = "HTTP/1.1 200 Connection Established\r\n\r\n";
        client_stream.write_all(response.as_bytes()).await?;
        forward_streams(client_stream, target_stream).await?;
    } else {
        send_error(&mut client_stream, 405, "Method Not Allowed").await?;
    }

    Ok(())
}
