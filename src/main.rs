use std::io::{self, Read, Write};
use std::iter::Iterator;
use std::net;
use std::sync;
use std::thread;

const POISONED_MUTEX_ERROR: &str = "Mutex::lock failed";
const SET_NONBLOCKING_ERROR: &str = "TcpStream::set_nonblocking failed";
const NONBLOCKING: bool = true;

lazy_static::lazy_static! {
    static ref LISTEN_ADDR: net::SocketAddr = net::SocketAddr::new(
        net::IpAddr::V4(net::Ipv4Addr::new(0, 0, 0, 0)), 20502
    );
    static ref CONNECT_TO_ADDR: net::SocketAddr = net::SocketAddr::new(
        net::IpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)), 20582
    );
}

fn incoming_connection_handle(source_stream: net::TcpStream, source_addr: net::SocketAddr) {
    source_stream
        .set_nonblocking(NONBLOCKING)
        .expect(SET_NONBLOCKING_ERROR);
    let source_stream = sync::Arc::new(sync::Mutex::new(source_stream));

    let destination_stream =
        net::TcpStream::connect(*CONNECT_TO_ADDR).expect("cannot connect to destination address");
    destination_stream
        .set_nonblocking(NONBLOCKING)
        .expect(SET_NONBLOCKING_ERROR);
    let destination_stream = sync::Arc::new(sync::Mutex::new(destination_stream));

    let cloned_source_stream = sync::Arc::clone(&source_stream);
    let cloned_destination_stream = sync::Arc::clone(&destination_stream);

    drop(thread::spawn(move || {
        let mut buffer = [0; 2048];

        'source_stream_handle: loop {
            let read_length = match source_stream
                .lock()
                .expect(POISONED_MUTEX_ERROR)
                .read(&mut buffer)
            {
                Ok(read_length) if read_length == 0 => continue 'source_stream_handle,
                Ok(read_length) => read_length,
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock) => {
                    continue 'source_stream_handle
                }
                Err(error) => panic!("Failed to read from {} due to {:?}", source_addr, error),
            };

            let write_length = match destination_stream
                .lock()
                .expect(POISONED_MUTEX_ERROR)
                .write(&buffer[0..read_length])
            {
                Ok(write_length) => write_length,
                Err(error) => panic!(
                    "Failed to write into {} due to {:?}",
                    *CONNECT_TO_ADDR, error
                ),
            };

            assert_eq!(read_length, write_length);

            let hexlified_source_data = hexlify(&buffer, write_length);
            println!("S > D {}", hexlified_source_data);

            clean_buffer(&mut buffer, write_length);
        }
    }));

    drop(thread::spawn(move || {
        let mut buffer = [0; 2048];

        'destination_stream_handle: loop {
            let read_length = match cloned_destination_stream
                .lock()
                .expect(POISONED_MUTEX_ERROR)
                .read(&mut buffer)
            {
                Ok(read_length) if read_length == 0 => continue 'destination_stream_handle,
                Ok(read_length) => read_length,
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock) => {
                    continue 'destination_stream_handle
                }
                Err(error) => panic!(
                    "Failed to read from {} due to {:?}",
                    *CONNECT_TO_ADDR, error
                ),
            };

            let write_length = match cloned_source_stream
                .lock()
                .expect(POISONED_MUTEX_ERROR)
                .write(&buffer[0..read_length])
            {
                Ok(write_length) => write_length,
                Err(error) => panic!("Failed to write into {} due to {:?}", source_addr, error),
            };

            assert_eq!(read_length, write_length);

            let hexlified_destination_data = hexlify(&buffer, write_length);
            println!("D > S {}", hexlified_destination_data);

            clean_buffer(&mut buffer, write_length);
        }
    }))
}

fn hexlify(buffer: &[u8], length: usize) -> String {
    let hexlified_data = (&buffer[0..length])
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<String>>()
        .join(":");

    format!("{} (length - {})", hexlified_data, length)
}

fn clean_buffer(buffer: &mut [u8], length: usize) {
    for b in buffer.iter_mut().take(length) {
        *b = 0_u8;
    }
}

fn main() {
    let listener = net::TcpListener::bind(*LISTEN_ADDR).expect("failed to bind tcp listener");

    'incoming: for maybe_stream in listener.incoming() {
        match maybe_stream {
            Err(error) => eprintln!("Failed to accept incoming connection due to {:?}", error),
            Ok(source_stream) => {
                let source_addr = match source_stream.peer_addr() {
                    Ok(source_addr) => source_addr,
                    Err(error) => {
                        eprintln!("Failed to receive peer address due to {:?}", error);
                        continue 'incoming;
                    }
                };
                println!("Incoming connection from {}", source_addr);
                incoming_connection_handle(source_stream, source_addr);
            }
        }
    }
}
