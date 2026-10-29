use super::*;

#[test]
fn channels_are_identified_by_role_not_accept_order() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let token = [7; 16];
    let connect = |role| {
        let mut stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        stream.write_all(&[role]).unwrap();
        stream.write_all(&token).unwrap();
        stream
    };
    let client_ack = connect(b'A');
    let client_control = connect(b'C');
    let client_data = connect(b'D');
    let (data, control, ack) =
        accept_all(&listener, &token, Instant::now() + Duration::from_secs(1)).unwrap();
    assert_eq!(data.peer_addr().unwrap(), client_data.local_addr().unwrap());
    assert_eq!(
        control.peer_addr().unwrap(),
        client_control.local_addr().unwrap()
    );
    assert_eq!(ack.peer_addr().unwrap(), client_ack.local_addr().unwrap());
}

#[test]
fn authentication_waits_for_a_fragmented_hello() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let token = [9; 16];
    let sender = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.write_all(b"D").unwrap();
        std::thread::sleep(Duration::from_millis(10));
        stream.write_all(&token).unwrap();
        stream
    });
    let (role, accepted) =
        accept(&listener, &token, Instant::now() + Duration::from_secs(1)).unwrap();
    assert_eq!(role, b'D');
    assert_eq!(
        accepted.peer_addr().unwrap(),
        sender.join().unwrap().local_addr().unwrap()
    );
}
