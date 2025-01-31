use crate::discovery::{DiscoveryService, Mode, PeerAddr};
use crate::peer_manager::PeerManagerEvent;
use ssb_crypto::PublicKey;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use termion::event::Key;
use termion::input::TermRead;

pub enum Event<I> {
    Input(I),
    Tick,
    NewPeer(PeerAddr),
    PeerManagerEvent(PeerManagerEvent),
}

pub struct Events {
    rx: mpsc::Receiver<Event<Key>>,
    _input_handle: thread::JoinHandle<()>,
    _tick_handle: thread::JoinHandle<()>,
    _new_peer_handle: thread::JoinHandle<()>,
    _pm_handle: thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub exit_key: Key,
    pub tick_rate: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            exit_key: Key::Char('q'),
            tick_rate: Duration::from_millis(250),
        }
    }
}

impl Events {
    pub fn new(public_key: PublicKey, peer_manager_rx: mpsc::Receiver<PeerManagerEvent>) -> Events {
        Events::with_config(public_key, peer_manager_rx, Config::default())
    }

    pub fn with_config(
        public_key: PublicKey,
        peer_manager_rx: mpsc::Receiver<PeerManagerEvent>,
        config: Config,
    ) -> Events {
        let (tx, rx) = mpsc::channel();
        let _input_handle = {
            let tx = tx.clone();
            thread::spawn(move || {
                let stdin = io::stdin();
                for evt in stdin.keys() {
                    match evt {
                        Ok(key) => {
                            if let Err(_) = tx.send(Event::Input(key)) {
                                return;
                            }
                            if key == config.exit_key {
                                return;
                            }
                        }
                        Err(_) => {}
                    }
                }
            })
        };
        let _tick_handle = {
            let tx = tx.clone();
            thread::spawn(move || loop {
                tx.send(Event::Tick).unwrap();
                thread::sleep(config.tick_rate);
            })
        };
        let _new_peer_handle = {
            let tx = tx.clone();
            let peer_listener = DiscoveryService::new(Mode::Debug, public_key).unwrap();
            thread::spawn(move || loop {
                if let Ok(ssb_peer) = peer_listener.recv() {
                    let _res = tx.send(Event::NewPeer(ssb_peer));
                }
            })
        };
        let _pm_handle = {
            let tx = tx.clone();
            thread::spawn(move || loop {
                if let Ok(pm_event) = peer_manager_rx.recv() {
                    let _res = tx.send(Event::PeerManagerEvent(pm_event));
                }
            })
        };

        Events {
            rx,
            _input_handle,
            _tick_handle,
            _new_peer_handle,
            _pm_handle,
        }
    }

    pub fn next(&self) -> Result<Event<Key>, mpsc::RecvError> {
        self.rx.recv()
    }
}
