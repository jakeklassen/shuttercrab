//! Both ends of a connection in one process, meeting through the real
//! meeting point: a room opened with the host key, joined by its link,
//! then a few seconds of data straight across. Needs the internet and
//! the host key, from `SHUTTERCRAB_HOST_KEY` or `meet/.env`; never printed.
//!
//!   cargo test -p shuttercrab-connect --test online -- --ignored --nocapture

use shuttercrab_connect::{Link, MEET, host, join};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

fn host_key() -> String {
    if let Ok(key) = std::env::var("SHUTTERCRAB_HOST_KEY") {
        return key;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../meet/.env");
    let text = std::fs::read_to_string(path).expect("SHUTTERCRAB_HOST_KEY or meet/.env");
    text.lines()
        .find_map(|line| line.strip_prefix("HOST_KEY="))
        .expect("HOST_KEY in meet/.env")
        .trim()
        .to_string()
}

#[test]
#[ignore = "meets through the real meeting point: run by hand"]
fn two_apps_meet_and_connect_directly() {
    let meet = std::env::var("SHUTTERCRAB_MEET").unwrap_or_else(|_| MEET.into());
    let hosting = host(&meet, &host_key()).unwrap_or_else(|e| panic!("{e:#}"));
    let link = Link::parse(&hosting.link().to_string()).unwrap();
    println!("link {link:?}");

    let started = Instant::now();
    let viewer = std::thread::spawn({
        let meet = meet.clone();
        move || join(&meet, &link)
    });
    let streaming = hosting.accept().unwrap_or_else(|e| panic!("{e:#}"));
    let viewer = viewer.join().unwrap().unwrap_or_else(|e| panic!("{e:#}"));
    println!(
        "connected in {} ms to {}, {} ms there and back",
        started.elapsed().as_millis(),
        streaming.remote(),
        streaming.round_trip().as_millis()
    );

    // Three seconds at about 20 Mbit/s, in pieces the size of pictures.
    let sender = std::thread::spawn(move || {
        let mut out = streaming.send(0).unwrap();
        let piece = vec![7u8; 40_000];
        let until = Instant::now() + Duration::from_secs(3);
        let mut sent = 0;
        while Instant::now() < until {
            out.write_all(&piece).unwrap();
            sent += piece.len();
            std::thread::sleep(Duration::from_millis(16));
        }
        drop(out);
        (sent, streaming)
    });
    let mut input = viewer.receive().unwrap();
    let (mut got, mut buffer) = (0, vec![0; 65536]);
    let reading = Instant::now();
    while got < 3 * 60 * 40_000 * 9 / 10 {
        let n = input.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        got += n;
    }
    let (sent, streaming) = sender.join().unwrap();
    let (lost, of) = streaming.lost();
    println!(
        "{got} of {sent} bytes in {:.1} s ({:.1} Mbit/s); {lost} of {of} packets lost",
        reading.elapsed().as_secs_f64(),
        got as f64 * 8.0 / reading.elapsed().as_secs_f64() / 1e6
    );
    assert!(got > 0);
}
